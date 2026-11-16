// Records one window with ScreenCaptureKit, even covered or on a hidden
// AeroSpace workspace, so a Rex take needn't take over the screen.
//
//   winrec WINDOW-ID size                 its size in points, W H
//   winrec WINDOW-ID shot OUT.png         a screenshot
//   winrec WINDOW-ID record OUT.mp4 FPS   a video, until SIGINT or SIGTERM
//
// The window id is the CGWindowID, as `aerospace list-windows` prints it.
// Both are the whole window at its own pixels: ScreenCaptureKit takes no
// source rectangle for one window, so its title bar is cropped after.
import AVFoundation
import CoreMedia
import Foundation
import ScreenCaptureKit

func fail(_ why: String) -> Never {
  FileHandle.standardError.write("winrec: \(why)\n".data(using: .utf8)!)
  exit(1)
}

func window(_ id: CGWindowID) async throws -> SCWindow {
  let content = try await SCShareableContent.excludingDesktopWindows(
    false, onScreenWindowsOnly: false)
  guard let w = content.windows.first(where: { $0.windowID == id }) else {
    fail("no window \(id)")
  }
  return w
}

func config(_ w: SCWindow, scale: CGFloat) -> SCStreamConfiguration {
  let c = SCStreamConfiguration()
  // even sizes, as H.264 wants
  c.width = Int(w.frame.width * scale) / 2 * 2
  c.height = Int(w.frame.height * scale) / 2 * 2
  c.showsCursor = false
  c.ignoreShadowsSingleWindow = true
  c.pixelFormat = kCVPixelFormatType_32BGRA
  return c
}

final class Recorder: NSObject, SCStreamOutput {
  let writer: AVAssetWriter
  let input: AVAssetWriterInput
  var started = false
  let queue = DispatchQueue(label: "winrec")

  init(out: URL, width: Int, height: Int) throws {
    try? FileManager.default.removeItem(at: out)
    writer = try AVAssetWriter(outputURL: out, fileType: .mp4)
    input = AVAssetWriterInput(
      mediaType: .video,
      outputSettings: [
        AVVideoCodecKey: AVVideoCodecType.h264,
        AVVideoWidthKey: width,
        AVVideoHeightKey: height,
        AVVideoCompressionPropertiesKey: [AVVideoAverageBitRateKey: 12_000_000],
      ])
    input.expectsMediaDataInRealTime = true
    writer.add(input)
    super.init()
  }

  func stream(
    _ stream: SCStream, didOutputSampleBuffer buffer: CMSampleBuffer, of type: SCStreamOutputType
  ) {
    guard type == .screen, buffer.isValid,
      let attachments = CMSampleBufferGetSampleAttachmentsArray(buffer, createIfNecessary: false)
        as? [[SCStreamFrameInfo: Any]],
      let raw = attachments.first?[.status] as? Int,
      SCFrameStatus(rawValue: raw) == .complete
    else { return }
    if !started {
      writer.startWriting()
      writer.startSession(atSourceTime: buffer.presentationTimeStamp)
      started = true
    }
    if input.isReadyForMoreMediaData {
      input.append(buffer)
    }
  }
}

let args = CommandLine.arguments
guard args.count >= 3, let id = CGWindowID(args[1]) else {
  fail("usage: winrec WINDOW-ID shot OUT.png | record OUT.mp4 FPS | size")
}
let scale = NSScreen.main?.backingScaleFactor ?? 2

Task {
  do {
    let w = try await window(id)
    switch args[2] {
    case "size":
      print(Int(w.frame.width), Int(w.frame.height))
      exit(0)
    case "shot":
      let image = try await SCScreenshotManager.captureImage(
        contentFilter: SCContentFilter(desktopIndependentWindow: w),
        configuration: config(w, scale: scale))
      let url = URL(fileURLWithPath: args[3]) as CFURL
      guard let dest = CGImageDestinationCreateWithURL(url, "public.png" as CFString, 1, nil)
      else { fail("can't write \(args[3])") }
      CGImageDestinationAddImage(dest, image, nil)
      CGImageDestinationFinalize(dest)
      exit(0)
    case "record":
      let c = config(w, scale: scale)
      let fps = Int32(args.count > 4 ? Int(args[4]) ?? 30 : 30)
      c.minimumFrameInterval = CMTime(value: 1, timescale: fps)
      c.queueDepth = 6
      let rec = try Recorder(out: URL(fileURLWithPath: args[3]), width: c.width, height: c.height)
      let stream = SCStream(
        filter: SCContentFilter(desktopIndependentWindow: w), configuration: c, delegate: nil)
      try stream.addStreamOutput(rec, type: .screen, sampleHandlerQueue: rec.queue)
      try await stream.startCapture()
      print("recording")
      fflush(stdout)
      let stop: @Sendable () -> Void = {
        Task {
          try? await stream.stopCapture()
          rec.queue.sync {
            rec.input.markAsFinished()
            rec.writer.finishWriting { exit(0) }
          }
        }
      }
      for sig in [SIGINT, SIGTERM] {
        signal(sig, SIG_IGN)
        let source = DispatchSource.makeSignalSource(signal: sig, queue: .main)
        source.setEventHandler(handler: stop)
        source.resume()
        sources.append(source)
      }
    default:
      fail("unknown mode \(args[2])")
    }
  } catch {
    fail("\(error)")
  }
}
nonisolated(unsafe) var sources: [DispatchSourceSignal] = []
dispatchMain()
