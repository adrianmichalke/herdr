" HERDR_INTEGRATION_ID=vim
" HERDR_INTEGRATION_VERSION=1

if exists('g:loaded_herdr_vim') || $HERDR_ENV !=# '1' || empty($HERDR_PANE_ID) || empty($HERDR_SOCKET_PATH) || !executable($HERDR_BIN_PATH)
  finish
endif
if $HERDR_VIM_ACTIVE ==# sha256($HERDR_SOCKET_PATH . "\n" . $HERDR_PANE_ID)
  finish
endif
if !exists('*timer_start') || !exists('*reltimefloat') || (!exists('*job_start') && !exists('*jobstart'))
  finish
endif
let g:loaded_herdr_vim = 1
let $HERDR_VIM_ACTIVE = sha256($HERDR_SOCKET_PATH . "\n" . $HERDR_PANE_ID)
let s:dirty = 1
let s:ready = 0
let s:closing = 0
let s:reporting = 0
let s:timer = -1
let s:timeout = -1
let s:seq = 0
let s:last_report = 0
let s:last_save = 0
let s:started = localtime()
let s:source = ''
let s:file = ''
let s:editor = has('nvim') ? 'nvim' : 'vim'

function! s:Sequence() abort
  let s:seq += 1
  return string(s:seq)
endfunction

function! s:StopJob() abort
  if !s:reporting
    return
  endif
  let s:reporting = 0
  if has('nvim')
    silent! call jobstop(s:job)
  else
    silent! call job_stop(s:job, 'kill')
  endif
endfunction

function! s:TimedOut(timer) abort
  let s:timeout = -1
  let s:last_report = 0
  call s:StopJob()
endfunction

function! s:Reported(job, code, ...) abort
  if a:job != s:job
    return
  endif
  let s:reporting = 0
  if s:timeout != -1
    call timer_stop(s:timeout)
    let s:timeout = -1
  endif
  if a:code != 0
    let s:last_report = 0
  endif
endfunction

function! s:Send(release) abort
  if s:reporting
    return
  endif
  let args = [$HERDR_BIN_PATH, 'pane', a:release ? 'release-agent' : 'report-agent', $HERDR_PANE_ID, '--source', s:source, '--agent', 'vim', '--seq', s:Sequence()]
  if !a:release
    call extend(args, ['--state', 'idle', '--'])
    if s:file =~# "'"
      let path = substitute(json_encode(s:file), "'", '\\x27', 'g')
      call extend(args, [s:editor, '-c', 'execute "source " . fnameescape(' . path . ')'])
    else
      call extend(args, [s:editor, '-S', s:file])
    endif
  endif
  let s:reporting = 1
  let s:last_report = localtime()
  if has('nvim')
    let s:job = jobstart(args, {'on_exit': function('s:Reported')})
    if s:job <= 0
      let s:reporting = 0
      let s:last_report = 0
      return
    endif
    call chanclose(s:job, 'stdin')
  else
    let s:job = job_start(args, {'in_io': 'null', 'out_io': 'null', 'err_io': 'null', 'exit_cb': function('s:Reported')})
    if job_status(s:job) ==# 'fail'
      let s:reporting = 0
      let s:last_report = 0
      return
    endif
  endif
  let s:timeout = timer_start(1000, function('s:TimedOut'))
endfunction

function! s:Save() abort
  let options = &sessionoptions
  let session = v:this_session
  let temporary = s:file . '.tmp'
  try
    noautocmd let &sessionoptions = 'blank,buffers,curdir,folds,help,tabpages,winsize'
    execute 'silent noautocmd mksession! ' . fnameescape(temporary)
    if !has('win32') && setfperm(temporary, 'rw-------') != 1
      return 0
    endif
    if rename(temporary, s:file) != 0
      return 0
    endif
    let s:dirty = 0
    let s:ready = 1
    let s:last_save = localtime()
    return 1
  catch
    return 0
  finally
    noautocmd let &sessionoptions = options
    let v:this_session = session
    call delete(temporary)
  endtry
endfunction

function! s:Checkpoint(timer) abort
  if s:closing || get(g:, 'SessionLoad', 0)
    return
  endif
  if s:dirty || localtime() - s:last_save >= 10
    call s:Save()
  endif
  let interval = localtime() - s:started < 10 ? 2 : 10
  if s:ready && !s:reporting && (s:last_report == 0 || localtime() - s:last_report >= interval)
    call s:Send(0)
  endif
endfunction

function! s:Start() abort
  try
    if !empty($XDG_STATE_HOME)
      let root = $XDG_STATE_HOME . '/herdr/vim'
    elseif has('win32') && !empty($LOCALAPPDATA)
      let root = $LOCALAPPDATA . '/herdr/vim'
    else
      let root = expand('~/.local/state/herdr/vim')
    endif
    call mkdir(root, 'p', 0700)
    if getftype(root) !=# 'dir'
      return
    endif
    if !has('win32') && setfperm(root, 'rwx------') != 1
      return
    endif
    let identity = strpart(sha256($HERDR_SOCKET_PATH . ':' . $HERDR_PANE_ID . ':' . getpid() . ':' . reltimestr(reltime())), 0, 32)
    let directory = root . '/' . identity
    if mkdir(directory, '', 0700) != 1
      return
    endif
    let s:file = directory . '/session.vim'
    let s:source = 'herdr-vim:' . identity
    call s:Checkpoint(-1)
    let s:timer = timer_start(2000, function('s:Checkpoint'), {'repeat': -1})
  catch
    return
  endtry
endfunction

function! s:Leave() abort
  let s:closing = 1
  if s:timer != -1
    call timer_stop(s:timer)
  endif
  if s:timeout != -1
    call timer_stop(s:timeout)
    let s:timeout = -1
  endif
  call s:StopJob()
  if !s:ready || v:dying
    return
  endif
  call s:Send(1)
  if s:reporting
    if has('nvim')
      call jobwait([s:job], 300)
    else
      let started = reltime()
      while job_status(s:job) ==# 'run' && reltimefloat(reltime(started)) < 0.3
        sleep 10m
      endwhile
    endif
    call s:StopJob()
  endif
endfunction

augroup HerdrVim
  autocmd!
  autocmd BufAdd,BufEnter,BufWinEnter,BufWritePost,BufDelete,BufFilePost,CursorMoved,CursorMovedI,TabEnter,WinEnter * let s:dirty = 1
  autocmd VimLeavePre * call s:Leave()
  for s:event in ['WinClosed', 'TabClosed', 'DirChanged', 'VimResized', 'WinResized', 'WinScrolled']
    if exists('##' . s:event)
      execute 'autocmd ' . s:event . ' * let s:dirty = 1'
    endif
  endfor
  if exists('v:vim_did_enter') && v:vim_did_enter
    call s:Start()
  else
    autocmd VimEnter * call s:Start()
  endif
augroup END
