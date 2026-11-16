" The demo daemons' clipboard: in Neovim's memory, never the system's. A
" yank and its paste are seconds apart in a take, and a copy made anywhere
" on the machine in between broke the paste; the takes no longer overwrite
" your clipboard either. bashrc's neopane function sources this with --cmd,
" before the config, which sets 'clipboard' but leaves g:clipboard alone.
let g:demo_clipboard = {'+': [[''], 'v'], '*': [[''], 'v']}
function! s:copy(reg, lines, regtype) abort
  let g:demo_clipboard[a:reg] = [a:lines, a:regtype]
endfunction
let g:clipboard = {
      \ 'name': 'demo (in memory)',
      \ 'copy': {
      \   '+': {lines, regtype -> s:copy('+', lines, regtype)},
      \   '*': {lines, regtype -> s:copy('*', lines, regtype)},
      \ },
      \ 'paste': {
      \   '+': {-> g:demo_clipboard['+']},
      \   '*': {-> g:demo_clipboard['*']},
      \ },
      \ }
