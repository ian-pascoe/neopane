# The daemon's lifetime: quitting, temp cleaners, parent death

Gathered for the decision on whether the daemon outlives its last terminal
while Neovim has unsaved changes, and on finding daemons later by a
predictable socket path. The daemon starts `nvim --headless --listen
<dir>/nvim.sock` with stdin and stdout null (`src/daemon/serve.rs:306-319`) and
keeps its files in `$XDG_RUNTIME_DIR`, else `$TMPDIR`, else `/tmp`
(`src/daemon/serve.rs:70-85`).

Sources read: Neovim `v0.12.6` (commit `779c3696`) and master (commit
`893d90fc`); systemd `v262.2`; tmux `3.8`; Apple's `system_cmds-597.90.1`
(the last tag that still has `dirhelper.c`); man pages; and the macOS system
files on this Mac (macOS 27.0.1, build 26A434). Behaviour was run against the
installed Neovim 0.12.6 on macOS with private `--listen` servers in a scratch
directory. Anything marked *source only* was read, not run; *unconfirmed*
means neither.

Links: Neovim 0.12 is `https://github.com/neovim/neovim/blob/v0.12.6/<file>#L<n>`
and master is the same with `893d90fced480d234e8eb1c3922c2ee5f898d3e4` in
place of the tag. Below, `file.c:N` is the 0.12.6 line unless it says
`master`; the logic cited is the same in master, only the line numbers move.
systemd is `https://github.com/systemd/systemd/blob/v262.2/<file>#L<n>`; tmux
is `https://github.com/tmux/tmux/blob/3.8/<file>#L<n>`; Apple's is
`https://github.com/apple-oss-distributions/system_cmds/blob/system_cmds-597.90.1/dirhelper.tproj/dirhelper.c#L<n>`.

## Answers

**R1. Does `:qa` refuse while a `:terminal` job runs? No.**

- `:qa` (without `!`) and `:q` on the last window quit and kill a running
  `:terminal` job; they don't refuse. A terminal buffer is never "changed"
  (`bufIsChanged` is false for it), and nothing else in `:qa` or `:q` checks
  for running jobs. Run on 0.12.6: `:q`, `:quit` and `:qa` each exited the
  server (status 0) and the `sleep` in the terminal was gone a second later.
- What does refuse is `:wqa`/`:xa` (E948, "Job still running (add ! to end the
  job)"), and `:bdelete`/`:bwipeout`/`:bunload` of the buffer (E89). `:wqa!`
  quits and kills the job. All run on 0.12.6.
- What makes `:qa` refuse is a modified, writable buffer: `E37` then `E162`
  ("No write since last change for buffer …"). Modified `nofile`, `nowrite`,
  `prompt` (unless 'modified' was set explicitly) and `terminal` buffers don't
  count.
- There is no dry-run for `:qa`. To predict it without trying it, list the
  buffers whose `getbufinfo(b)[1].changed` is 1 (that is `bufIsChanged`, the
  same test `:qa` uses). `vim.bo[b].modified` agrees. Don't use
  `getbufinfo({bufmodified = 1})`: it tests the raw flag, and a terminal
  buffer has it set, so it lists terminals (observed). For `:wqa`/`:xa`, also
  check for a terminal buffer whose job runs: `jobwait({chan}, 0)[1] == -1`.
  `v:dying` is only non-zero while exiting on a deadly signal, and `v:exiting`
  is `v:null` until exit begins, so neither helps before the fact. A
  `QuitPre`/`ExitPre` autocommand can also abort the quit, and that can't be
  seen from outside.
- Headless, with no UI: `:qa` on a modified buffer prints E37 and E162 and
  does not quit; nothing prompts or blocks (observed over RPC and on the
  command line). Through `nvim_exec2`/`vim.cmd` it arrives as a thrown error
  (the E37 text).
- But with `'confirm'` set (or `:confirm qa`) and no UI attached, the "Save
  changes?" dialog answers itself with its default, **Yes**: the buffer is
  written, and an unnamed one is named `Untitled` in the current directory
  (observed: `Untitled` appeared, the server exited 0). With a UI attached,
  as the daemon is, the same command waits for a key (mode `r?`; observed
  until `<Esc>`).

**R2. Do temp cleaners delete the socket or its directory?**

- **macOS `/tmp`:** no. `/usr/libexec/tmp_cleaner` runs daily at 00:00, and
  deletes only regular files (`find -type f`) whose atime, mtime and ctime are
  all over 3 days old, and empty directories over 3 days old. Sockets are never
  deleted, so a directory holding a socket is never empty.
- **macOS `$TMPDIR`** (`/var/folders/xx/yyyy/T`): the socket and its directory
  survive; **regular files in it don't necessarily.** `dirhelper` runs daily at
  03:35 with `CLEAN_FILES_OLDER_THAN_DAYS=3`. In the source (597.90.1) it
  deletes only regular files, when birth time and atime are both over 3 days
  old. mtime is not looked at, so a log that is only ever written can be
  deleted while open. It skips a file it can't `flock` exclusively. It touches
  directories and other file types only at boot (`-machineBoot`), when
  everything in `T` goes. The shipped binary's strings and launchd plist match
  the source, but its code wasn't read. This Mac: 12 sockets older than 3 days
  (9 older than 10 days) and 3,503 old directories sit in `$TMPDIR`, while 1 of
  13,079 regular files has an atime over 3 days.
- **Linux `/tmp`, systemd-tmpfiles:** upstream's default is `q /tmp 1777 root
  root 10d`, run by a daily timer (the first run 15 minutes after boot).
  Age is the newest of atime, mtime and ctime; a **socket listed in
  `/proc/net/unix` is skipped**; a directory is removed only when old *and*
  empty; a file or directory that holds a BSD lock (`flock`) is skipped. Not
  every distro cleans: Debian 13 does so only on new installs, and upgraded
  systems keep a boot-only clear. A distro that sets `D /tmp 1777 root root -`
  never ages (not read for Fedora/Ubuntu: unconfirmed).
- **Linux `$XDG_RUNTIME_DIR`: yes, it goes with the last logout, even if
  processes survive.** `user-runtime-dir@UID.service` stops when the user's
  last session ends (after `UserStopDelaySec=` 10 s) unless lingering is on.
  Its stop deletes the directory's contents, then unmounts and removes it. The
  daemon's socket file is gone and the daemon is unreachable by path. Whether
  the daemon survives depends on `KillUserProcesses=`: upstream defaults to
  `yes` (processes in the session scope are killed at logout); Debian and
  Fedora build with `no`. `RemoveIPC=` (default `yes`) removes SysV/POSIX IPC
  objects, not files, and is no threat to a Unix socket. Linger keeps the user
  manager and `/run/user/UID`, and processes started under it. XDG's own spec
  says the directory's lifetime is bound to the login and files in it may be
  aged out unless their atime is touched every 6 hours or the sticky bit is
  set.
- **tmux** keeps `$TMUX_TMPDIR/tmux-UID/<name>`, else `/tmp/tmux-UID/<name>`
  (`TMUX_SOCK` is `"$TMUX_TMPDIR:" _PATH_TMP`). It doesn't notice a removed
  socket: `SIGUSR1` to the server makes it re-create the socket, and that is
  manual and fails if a parent directory is missing. So tmux relies on `/tmp`
  cleaners sparing sockets (they do, above) and on a manual fix otherwise.

**R3. Can headless Neovim exit when its parent daemon dies, even by SIGKILL,
keeping its swap files? Yes: `nvim --embed --headless`.**

- With `--embed`, the stdin/stdout pair is an RPC channel, and **when it
  closes Neovim exits** (documented: "If the channel is closed (except by
  `:detach`), Nvim exits"). The exit goes through `preserve_exit()`: it syncs
  every swap file and closes them without deleting, then runs the normal exit
  (autocommands `VimLeavePre`/`VimLeave`, status 1). Run: SIGKILL of a Python
  parent holding the stdin pipe made Neovim exit within 2 s, leaving a swap
  file that held an edit made over the socket after startup, and `VimLeave`
  ran (`v:dying` 0). The kernel closes the pipe on any death, so SIGKILL
  works.
- `--headless` alone does **not** do this: a pipe on stdin is not read in
  headless mode, so closing it did nothing (observed: still running). A
  `--listen` socket client disconnecting doesn't exit Neovim either (observed).
- A watcher in Lua can't match it. A `vim.uv` pipe on stdin that runs `:qa!` on
  EOF exits cleanly, which **deletes the swap files** (observed: no swap left).
  Only `vim.uv.kill(vim.uv.os_getpid(), "sighup")` keeps them (observed).
- Linux `prctl(PR_SET_PDEATHSIG, SIGHUP)` would work for Neovim, but it fires
  when the parent *thread* exits, not the process, and has no macOS or Windows
  equivalent. macOS has no such call (nothing in the man pages or SDK headers
  found: *unconfirmed* as a negative); a watcher process can use kqueue
  `EVFILT_PROC`/`NOTE_EXIT` on the parent. Windows: a job object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` kills Neovim, with no chance to save a
  swap file.
- **Recommendation:** spawn `nvim --embed --headless --listen <sock>`, with a
  piped stdin the daemon holds open and never writes to, stdout null, stderr
  to the log. It's one mechanism for graceful shutdown and SIGKILL, it's in
  Neovim's own code, and it uses nothing platform-specific. Details and
  caveats at the end.

## R1 evidence

### `:qa` and `:q` never look at terminal jobs

- `ex_quitall` calls `check_changed_any(false, false)` unless `!`
  (`ex_docmd.c:4961-4972`; master `:4866`), after `before_quit_all`, which
  only runs `QuitPre`/`ExitPre` and refuses in the command-line window or when
  text is locked (`ex_docmd.c:4938-4958`).
- `ex_quit` quits when `check_changed` on the window's buffer (skipped under
  'hidden'), `check_more` and `check_changed_any(forceit, true)` all pass, via
  `getout(0)` (`ex_docmd.c:4862-4923`; master `:4778`). `check_more` is the
  "N more files to edit" check of the argument list (`ex_docmd.c:4751-4775`).
- `check_changed_any` walks the buffers (current, then those in windows, then
  the rest) and stops at the first with `bufIsChanged(buf)`
  (`ex_cmds2.c:310-376`, the test at `:358`; master `:314`). Only then does it
  report the error.
- `bufIsChanged` is `!bt_dontwrite(buf) && (b_changed || file_ff_differs)`
  (`undo.c:3111-3118`; master `:3220`) and `bt_dontwrite` is true for
  `nofile`/`nowrite`, `prompt` and **any `buf->terminal`**
  (`buffer.c:4041-4047`; master `:4103`). So terminal buffers are never
  changed, whatever the job does. A prompt buffer counts only when 'modified'
  was set explicitly (`undo.c:3114-3116`).
- The E947 branch of `check_changed_any` ("Job still running in buffer",
  `ex_cmds2.c:390-391`; master `:394-395`) is reached only for a buffer that
  `bufIsChanged` said was changed, and a terminal buffer never is; so it looks
  unreachable. Inferred from the code, not shown to be dead by a test.
- The terminal text itself sets the raw flag: `terminal.c:2567` calls
  `changed_lines`, so `buf->b_changed` is true. That is why
  `getbufinfo({bufmodified = 1})` lists a terminal (`eval/buffer.c:658` tests
  `b_changed`) while `getbufinfo(b)[1].changed` is 0 (`eval/buffer.c:582`
  uses `bufIsChanged`).
- On exit the jobs are stopped, not waited for: `event_teardown` runs
  `proc_teardown`, which `proc_stop`s ordinary child processes and closes the
  handles of pty ones (the pty master closing hangs the child up)
  (`main.c:171-189`, `event/proc.c:131-147`). Observed: the `sleep` was gone
  after every quit.

### What refuses on a running job

- `do_wqall`, for `:wqa` and `:xa`, errors for any buffer with a running job
  when exiting and no `!`: `no_write_message_buf` gives E948
  (`ex_cmds.c:2099-2120`; master `:2112-2131`; the message is
  `errors.h:141-142`). Neovim's own test asserts it:
  `test/functional/terminal/buffer_spec.lua` ("requires bang (!) to close a
  running job #15402") and `:wqall! closes a running job`. Run here:
  `Vim(wqall):E948`, `Vim(xall):E948`, then `wqa!` exited.
- Closing the buffer: `do_buffer` refuses with E89 ("%s will be killed (add !
  to override)", or a dialog under 'confirm'), `buffer.c:1424-1432` (master
  `:1450-1458`).
- `channel_job_running` is the test used, `channel.c:994-1002`: false for
  `nvim_open_term()` terminals, which have no process (its own comment, and
  the `TODO` at `ex_cmds.c:2115-2116`).

### Probe (run on 0.12.6)

```lua
local function qa_blockers()
  local r = { modified = {}, jobs = {} }
  for _, b in ipairs(vim.fn.getbufinfo()) do
    if b.changed == 1 then r.modified[#r.modified + 1] = b.bufnr end -- :qa refuses
    local ch = vim.bo[b.bufnr].buftype == 'terminal' and vim.bo[b.bufnr].channel or 0
    if ch ~= 0 and vim.fn.jobwait({ ch }, 0)[1] == -1 then
      r.jobs[#r.jobs + 1] = b.bufnr -- only :wqa/:xa and :bdelete refuse
    end
  end
  return r
end
```

Results: with only a running terminal, `modified = {}`, `jobs = {1}`; after
adding a modified ordinary buffer, `modified = {2}`; a further modified
`nofile` buffer was not listed. `vim.v.dying` was 0 and
`vim.v.exiting` was `v:null` throughout.

### Headless, unsaved changes

- `check_changed_any` prints the error unless 'confirm' or `:confirm` is in
  force (`ex_cmds2.c:379-403`): first `check_changed` emits E37
  (`ex_cmds2.c:149-186`, the emit at `:180-184`), then E162 (`:392-394`). The
  "Press ENTER" wait after it is skipped without a UI (`wait_return` returns
  early when `headless_mode && !ui_active()`, `message.c:1433`; master
  `message.c:1421`). Run on the
  command line with `timeout 3 nvim --headless --clean -c enew -c 'call
  setline(1,"x")' -c qa </dev/null`:

  ```
  Error in command line:
  E37: No write since last change
  E162: No write since last change for buffer "[No Name]"
  ```

  followed by the script's next command (it kept running); the timeout's
  SIGTERM made Neovim print `Nvim: preserving files...`.
- With 'confirm': `check_changed` calls `dialog_changed`
  (`ex_cmds2.c:149-186`, `:195-260`), which asks "Save changes to …?" with
  Yes as the default and, on Yes, names an unnamed buffer `Untitled` and
  writes it (`ex_cmds2.c:213-217`). `do_dialog` takes the default without
  asking when no UI is attached and no input is pending
  (`message.c:3746-3749`; master `dialog.c:157-162`, which also does it when
  `input_unanswerable()`, i.e. dying or exiting without a UI). Run: with
  `set confirm` in a headless server, `:qa` over RPC wrote `Untitled` and the
  server exited 0. With a UI attached (`nvim_ui_attach`), `:qa` left the
  server in mode `r?` until `<Esc>`, and stayed alive.
- The daemon is a UI, so under its attachment 'confirm' means a prompt that
  nobody answers. Not tested with the daemon's `ext_*` options (*unconfirmed*
  for those).

## R2 evidence

### macOS

- `/usr/libexec/tmp_cleaner` is a shell script, launched by
  `/System/Library/LaunchDaemons/com.apple.tmp_cleaner.plist` at `Hour 0`.
  It sets `daily_clean_tmps_dirs="/tmp"`, `daily_clean_tmps_days="3"` and
  runs `find -dx . -fstype local -type f -atime +3 -mtime +3 -ctime +3 -delete`
  then `find -dx . -fstype local ! -name . -type d -empty -mtime +3 -delete`,
  skipping `.X*-lock`, `.X11-unix` and similar names. `/tmp` is a symlink to
  `private/tmp`.
- `/usr/libexec/dirhelper` is started by
  `/System/Library/LaunchDaemons/com.apple.bsd.dirhelper.plist`
  (`StartCalendarInterval` `Hour 3`, `Minute 35`; environment
  `CLEAN_FILES_OLDER_THAN_DAYS=3`). `launchctl print
  system/com.apple.bsd.dirhelper` shows the environment. `man dirhelper` only
  says it creates special directories.
- The logic is from `dirhelper.c` in `system_cmds-597.90.1`
  (`dirhelper.tproj/dirhelper.c`): `clean_files_older_than` walks with `fts`
  (`:195-199`) and, for a non-boot run, deletes an entry only if
  `S_ISREG` and `st_birthtime < when` and `st_atime < when` (`:212`), after
  taking `flock(fd, LOCK_EX | LOCK_NB)` and checking again (`:218-228`); other
  entries are left. Symlinks and directories are removed only when `when == 0`,
  the boot run (`:245-250`, `:463-474`). `when` is now minus
  `CLEAN_FILES_OLDER_THAN_DAYS` (default 3) days (`:312-321`). The boot run
  also removes `Cleanup At Startup`. The cleaned subdirectory is the
  `DIRHELPER_TEMP_STR` name (`:466`, `:477`), which is the `T` of
  `/var/folders/xx/yyyy/T` (inferred from the path, the header wasn't read).
  The source is old: the shipped binary contains the same format strings
  ("Cleaning %s older than %ld days", `CLEAN_FILES_OLDER_THAN_DAYS`, "Cleanup
  At Startup", `-cleanTemporaryItems`), but the current code wasn't read, so
  treat the rules as *likely*, not confirmed. `/usr/libexec/dirs_cleaner`
  also exists (its strings mention `/private/var/dirs_cleaner` and `/tmp`; what
  calls it, and what it does to `$TMPDIR`, wasn't found: *unconfirmed*).
- On this Mac `$TMPDIR` is `/var/folders/7s/w381rjns5_7blt6vhvqs6nk80000gn/T/`;
  `find` over it found 13,079 regular files, 1 with atime over 3 days, 3,503
  directories with mtime over 3 days, and sockets from 18 days ago
  (`dotnet-diagnostic-*-socket`, Oct 18) still present. That fits "regular
  files only".
- atime is not set by writes. A log the daemon writes but nobody reads keeps
  its creation atime, and so `dirhelper`, which doesn't look at mtime, may
  delete it after 3 days (the daemon keeps writing to an unlinked file).
  Whether APFS here updates atime on a later read was not tested.
  A shared or exclusive `flock` held on the file would stop it (source only).

### Linux systemd-tmpfiles

- Default `/tmp` rule: `q /tmp 1777 root root 10d` and `q /var/tmp 1777 root
  root 30d` (`tmpfiles.d/tmp.conf:11-12`). `q` is `d` (create) plus `age`
  clean-up. The clean-up timer is `OnBootSec=15min`, `OnUnitActiveSec=1d`
  (`units/systemd-tmpfiles-clean.timer:16-17`).
- Age rule (`man/tmpfiles.d.xml:690-716`, `src/tmpfiles/tmpfiles.c:633-684`):
  any of mtime, atime, ctime newer than the cutoff keeps a file; for
  directories ctime is ignored by default, since the clean-up itself changes
  it.
- Sockets: `dir_cleanup` skips a socket that appears in `/proc/net/unix`
  (`tmpfiles.c:482-546`, `:852-856`), so a socket a daemon is listening on
  isn't removed; a socket nobody listens on is aged like any file. If
  `/proc/net/unix` can't be read, it assumes alive (`:542-543`).
- Directories: removed only if old and `rmdir` succeeds; `ENOTEMPTY` is ignored
  (`tmpfiles.c:807-826`).
- Locks: an entry on which it finds a BSD lock is skipped, with everything
  under it (directories `tmpfiles.c:784-788`, files `:877-881`;
  `man/tmpfiles.d.xml:720-737` documents it for applications). The sticky bit
  also exempts a file (the comment at `tmpfiles.c:834-837` cites the XDG
  spec).
- Not every system cleans `/tmp`: Debian 13's NEWS says cleaning happens "on
  new installs", with `D /tmp 1777` kept for upgrades, and `/tmp` is a tmpfs
  by default (`debian/NEWS:12-51`, systemd 257.13-1~deb13u1:
  `https://sources.debian.org/data/main/s/systemd/257.13-1~deb13u1/debian/NEWS`).
  Other distros' shipped `tmp.conf` weren't read: *unconfirmed*.

### `$XDG_RUNTIME_DIR` and logout

- `pam_systemd`: the directory "is automatically created the first time a user
  logs in and removed on the user's final logout"; contents are lost between
  logins (`man/pam_systemd.xml:321-342`). The XDG Base Directory spec (0.8,
  "Environment variables") says the same, that files may be aged out unless
  their atime moves every 6 hours or the sticky bit is set, and that the
  directory must not survive a full logout (`https://specifications.freedesktop.org/basedir/latest/`).
- Removal: `user-runtime-dir@.service` has `ExecStop=systemd-user-runtime-dir
  stop %i` (`units/user-runtime-dir@.service.in`). That runs
  `user_remove_runtime_path`: `rm_rf` the contents, `umount2(MNT_DETACH)`, then
  `rm_rf(REMOVE_ROOT)` (`src/login/user-runtime-dir.c:156-177`, `:179-200`).
  It stops with the user's last session, after `UserStopDelaySec=` (default
  10 s) (`man/logind.conf.xml:189-196`), unless linger keeps the user manager
  (`man/loginctl.xml:186-198`).
- Processes: `KillUserProcesses=` "configures whether the processes of a user
  should be killed when the user logs out" (`man/logind.conf.xml:104-127`).
  Upstream's default became `yes` in v230 (`NEWS:17183-17203`, which says
  long-lived processes need `systemd-run` under `user@.service` or lingering);
  Debian passes `-Ddefault-kill-user-processes=false`
  (`debian/rules:79`) and Fedora's spec also sets it (`systemd.spec:956`,
  `rawhide` when read). So whether the daemon outlives the logout varies by
  distro; the runtime directory doesn't.
- `RemoveIPC=` (default `yes`, `man/logind.conf.xml:373-380`) concerns System V
  and POSIX IPC objects only; files and Unix sockets in `/run/user/UID` are
  removed by the mechanism above, not by it.

### tmux

- `tmux.h:94-95` `TMUX_SOCK` is `"$TMUX_TMPDIR:" _PATH_TMP`;
  `make_label` takes the first usable entry, makes `<dir>/tmux-UID` with mode
  `0700` and refuses it if it isn't a directory owned by the user or is
  accessible to others (`tmux.c:204-249`). `tmux.1:161-191` documents `-L`,
  `TMUX_TMPDIR` (also `tmux.1:9328`) and `/tmp`.
- "If the socket is accidentally removed, the `SIGUSR1` signal may be sent to
  the tmux server process to recreate it (note that this will fail if any
  parent directories are missing)" (`tmux.1:187-191`). In the server, `SIGUSR1`
  creates a new socket, closes the old fd and keeps accepting (`server.c:450-458`);
  nothing in the server does it by itself. `server_update_socket`
  (`server.c:335-369`) only toggles the socket's execute bits by whether a
  session is attached.

## R3 evidence

### `--embed`

- `--embed` doc: "Use stdin/stdout as a msgpack-RPC channel … If the channel
  is closed (except by `:detach`), Nvim exits." Non-UI embedders must also pass
  `--headless` so startup doesn't wait for `nvim_ui_attach`; `nvim --embed
  --headless` is documented as equivalent to `nvim --headless --cmd "call
  stdioopen({'rpc': v:true})"` (`runtime/doc/starting.txt:381-403`). `--headless`
  and `--embed` both stop stdin being read as text (`starting.txt:42`).
  `:detach` leaves the server running (`runtime/doc/gui.txt:62-66`).
- The channel is made in `main.c:322-326` (`channel_from_stdio`); with
  `--embed` and not `--headless` startup waits for a UI (`main.c:438-444`).
  `channel_from_stdio` moves stdin and stdout to fds above 2 with
  `F_DUPFD_CLOEXEC`, then points fds 0 and 1 at stderr so child processes
  (`:terminal` jobs) never hold the pipe (`channel.c:590-597`; master
  `:576-583`).
- Closing: `rpc_close_event` calls `exit_on_closed_chan(0)` when the channel
  is the stdio one and wasn't detached (`msgpack_rpc/channel.c:515-516`; master
  `:560-562`). A socket channel falls into neither branch, so a `--listen`
  client disconnecting does nothing, as observed.
- `exit_on_closed_chan` queues `exit_event`, which, if Neovim isn't already
  exiting, calls `preserve_exit(NULL)` (`event/proc.c:417-441`; master
  `:418-440`).
- `preserve_exit` closes unmodified buffers, calls `ml_sync_all` to write every
  swap file, `ml_close_all(false)` (close without deleting) and `getout(1)`
  (`main.c:884-930`; master `:910`). `getout` runs `VimLeavePre` and
  `VimLeave` and writes ShaDa (`main.c:747-870`). On a normal exit
  `os_exit` calls `ml_close_all(true)`, which deletes swap files
  (`main.c:717-733`): hence a clean `:qa!` loses the unsaved text.
- Signals take the same route: SIGHUP, SIGINT, SIGTERM and SIGQUIT run
  `deadly_signal`, which sets `v:dying` and calls `preserve_exit`
  (`os/signal.c:189-206`, `:230-238`). The existing daemon's SIGHUP on drop
  relies on this (`src/daemon/serve.rs:198-255`).

### Runs (0.12.6, macOS; Python parent holding stdin as a pipe, then `kill -9`)

| setup | after SIGKILL of the parent |
| --- | --- |
| `nvim --embed --headless --listen …`, file open | exited within 2 s, swap file kept, `VimLeave` ran |
| same, with an edit made over the socket after start (swap file lacked it, `updatecount` 200) | the swap file then held the edit |
| `nvim --headless --listen …`, stdin a pipe | still running |
| socket client connected and disconnected | still running |
| `--headless` + `vim.uv` stdin watcher running `:qa!` on EOF | exited, **no swap file left** |
| same watcher running `vim.uv.kill(os_getpid(), "sighup")` | exited; stderr "Nvim: preserving files…", swap file kept |

Not run: the Linux and Windows paths of the same code; a `vim.uv`
`os_getppid()` poll as a fallback (*unconfirmed*).

### Other mechanisms

- `PR_SET_PDEATHSIG`: "the signal that the calling process will get when its
  parent dies", sent when the *parent thread* terminates; it is cleared across
  `fork` and for set-uid/capability binaries, and kept across `execve`
  (`https://man7.org/linux/man-pages/man2/PR_SET_PDEATHSIG.2const.html`). A
  daemon that spawns Neovim from a worker thread that later exits would kill
  Neovim early. Linux only; set it to SIGHUP, not SIGKILL, to keep swap files.
- macOS: `man 2 kqueue` documents `EVFILT_PROC` with `NOTE_EXIT` ("The process
  has exited") for watching another process, which needs a separate watcher
  process or thread to act on it. Neovim can't do that itself. No
  `PR_SET_PDEATHSIG` counterpart was found (`grep PDEATHSIG` over the SDK
  headers found nothing; *unconfirmed* as a general negative).
- Windows: a job object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` terminates
  its processes when the last handle closes
  (`https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-jobobject_basic_limit_information`):
  works for SIGKILL of the daemon, but with no swap-file save.
- The embed code has Windows branches (`channel.c:560-588`, ConPTY handling)
  that keep the original stdin pipe; not run.

### Recommendation and caveats

Use `--embed --headless` with a held stdin pipe:

- Spawn with `.stdin(Stdio::piped())`, keep the `ChildStdin` for the daemon's
  life, never write; stdout null; stderr the log. Drop it at graceful shutdown
  to get the same swap-preserving exit as today's SIGHUP (status 1, not 0),
  or keep sending SIGHUP and let the pipe be the backstop for crashes and
  SIGKILL.
- The write end must not leak: any other process holding it keeps Neovim
  alive. `Stdio::piped()` fds are close-on-exec; check anything the daemon
  forks without `exec`.
- Neovim now has an RPC channel on stdio; `rpcnotify(0, …)` would also write to
  it (stdout null swallows that). `windows.lua` notifies a specific `chan`
  (`src/nvim/windows.lua:4`, `:438`), so it isn't affected. Not run with the
  daemon itself.
- This ties the lifetimes: if the daemon dies, Neovim goes too, and unsaved
  changes survive only as swap files, recovered with `:recover`. If the
  decision is that Neovim must outlive the daemon while changes are unsaved,
  don't use it; keep stdin null, and use the R1 probe (`modified = {}`) as the
  test for whether it is safe to let a daemon or Neovim go.

## R4: `:detach` when the daemon is the `--embed` stdio UI

Same setup as R3: `nvim --embed --headless --listen SOCK`, the daemon attached
with `nvim_ui_attach` over stdio. Run on 0.12.6 with a Neovim parent
(`jobstart(..., {rpc = true})`) as the embedder and a second client on the
socket.

**Answers**

- `:detach` is not limited to remote TUIs, and it does not refuse. It
  detaches the "current UI" and closes that UI's RPC channel, here the stdio
  one, and **Neovim keeps running**, reachable only by `--listen`. Observed:
  after `:detach` (typed with `nvim_input`) the embedder got `Invalid
  channel` for further requests, `nvim_list_uis()` was empty, the process was
  alive and a socket client still worked.
- It does not run the exit path: no `ExitPre`, `VimLeavePre`, `QuitPre` or
  swap preservation. Nothing is written. The only event is `UILeave`, and it
  fires after the detach, so it can't veto it.
- The "current UI" is the channel that last attached, gained focus
  (`nvim_ui_set_focus(true)`) or sent input. So **any RPC client can trigger
  it**: `nvim_command("detach")` from a socket client closed the daemon's
  stdio channel (observed). `nvim_ui_detach` from the embedder itself only
  detaches the UI and leaves the channel open (observed: `UILeave` fired, the
  embedder's request still worked).
- It can be blocked only for typed use, not by a user command: `:detach` is
  built in and `:command! detach` is E183 (user commands start with an
  uppercase letter; observed). `cnoreabbrev detach …` and a `CmdlineLeave`
  autocommand setting `v:event.abort = v:true` both stopped a typed `:detach`
  (the UI stayed attached; observed), but neither runs for
  `nvim_command("detach")` from RPC, which detached as usual (observed). No
  event runs before the detach.
- Without a UI attached (a plain `--headless --listen`), `:detach` from a
  client says "UI not attached" and does nothing (observed).
- Consequence for the R3 pipe design: once the stdio channel is closed by
  `:detach`, the stdin pipe no longer ties Neovim to the daemon; Neovim
  outlives the daemon. The embedder gets a UI `error_exit` event, then EOF
  (source: `api/ui.c:95-99`), which is the daemon's cue; re-attaching over the
  socket was not tried (*unconfirmed*).

**Source (0.12.6)**

- `ex_detach` (`ex_docmd.c:6003-6063`): fails with "UI not attached" when
  `current_ui` is 0 (`:6018-6021`); sets the channel's `detach` flag via
  `nvim__chan_set_detach` (`:6028-6031`; `api/vim.c:1748-1764`, an internal
  `nvim__` API any remote client can call); `remote_ui_disconnect(..., true)`
  sends `error_exit` and removes the UI (`:6033-6037`; `api/ui.c:89-109`);
  then `channel_close(chan->id, kChannelPartAll)` (`:6039-6044`).
- `detach` stops the self-exit: `rpc_close_event` calls `exit_on_closed_chan`
  for a stdio channel only `&& !channel->detach`
  (`msgpack_rpc/channel.c:515`; `channel.h:34-36`: "Prevents self-exit on
  channel-close").
- `current_ui` is set in `nvim_ui_attach` (`api/ui.c:208`), `nvim_ui_set_focus`
  (`:237`) and `input_enqueue` (`os/input.c:277`).
- `:detach` is defined with `flags = TRLBAR`, no bang and no range
  (`ex_cmds.lua:742-746`); the "bang (!) not supported yet" branch
  (`ex_docmd.c:6011-6012`) is unreachable, and `detach!` gave E477 (observed).
- `UILeave` runs from `ui_detach_impl` after the UI is removed
  (`ui.c:415-451`, `autocmd.c:2683-2700`; `runtime/doc/autocmd.txt:1301-1305`).
- **Master differs.** `:detach!` there sets `chan->detach = true` (Neovim
  "will continue running if the UI disconnects") and `:%detach` detaches the
  other UIs (`ex_docmd.c:5873-5917` at commit `893d90fc`; `ex_cmds.lua:756-760`).
  So on master a user typing `:detach!` turns off the exit-on-pipe-close of R3
  for the daemon's channel; 0.12.6 has the same switch only through
  `nvim__chan_set_detach`. Not run on master.

## R5: what `preserve_exit()` runs after the stdio channel closes

The path: `rpc_close_event` → `exit_on_closed_chan(0)` → `exit_event` →
`preserve_exit(NULL)` if not already exiting (`event/proc.c:417-441`), as in
R3. Everything below is `main.c` at 0.12.6; master is the same, at
`preserve_exit` `:910-960`. Run: embedder SIGKILLed; Neovim with `-u NONE -i
x.shada`, two files (one modified over RPC), a `:terminal` job, and
autocommands logging events.

In order:

1. `errmsg` is NULL, so nothing is printed (`main.c:907-910`). Further
   SIGHUPs are ignored, `signal_reject_deadly()` (`:901`).
2. **`ml_close_notmod()`** (`main.c:915`): for each buffer that isn't
   `bufIsChanged`, `ml_close(buf, true)`: the memfile is closed and **its swap
   file deleted** (`memline.c:601-608`, `:567-580`). That includes `terminal`
   buffers, which are never "changed". Run: the unmodified file's swap was
   gone, the modified one's kept.
3. **`ml_sync_all(false, false, true)`** (`main.c:917-923`, `memline.c:1729-1759`):
   only if some buffer still has a swap file; flushes and fsyncs the changed
   buffers' swap files. This is what keeps the unsaved text.
4. **`ml_close_all(false)`** (`main.c:927`, `memline.c:590-597`): closes the
   remaining memfiles *without deleting*, and removes the temp directory.
   This sets every buffer's `ml_mfp` to NULL (`memline.c:579`).
5. **`getout(1)`** (`main.c:933`; `:747-880`), with `v:dying` 0 so the
   `v_dying <= 1` blocks run:
   - `BufWinLeave` for each buffer shown in a window, once per buffer
     (`:776-802`). Observed.
   - `BufUnload` loop (`:804-815`) only for buffers whose `ml_mfp` is not NULL.
     Step 4 cleared them all, so **`BufUnload` does not fire** (observed:
     absent).
   - `VimLeavePre` (`:817-828`), then **ShaDa is written**
     (`shada_write_file`, `:830-836`) if 'shada' is set; observed: the file was
     written and held a register set before the kill. Then `VimLeave`
     (`:839-849`). Both observed.
   - `ExitPre` and `QuitPre` are not run (they belong to `:quit`/`:qall`);
     observed absent. `v:exiting` is set to 1.
   - `os_exit(1)` (`:717-733`; `:731` calls `ml_close_all(true)`, a no-op now
     that every memfile is closed) then `event_teardown` (`:171-189`).
6. **Jobs are stopped last**, in `event_teardown`: `proc_teardown` stops
   ordinary children, and for pty (`:terminal`) jobs closes the handles so the
   pty hangs the child up (`event/proc.c:131-147`). Observed: the `sleep` was
   gone, and `TermClose` fired after `VimLeave`.

So the saved state is: modified buffers' swap files (recover with `:recover`),
ShaDa, and nothing for unmodified buffers (their swap files are deleted, which
is correct: the file on disk is current). Unsaved changes in a terminal or
`nofile` buffer are not preserved. The exit status is 1.
