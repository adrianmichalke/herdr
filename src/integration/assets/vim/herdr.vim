" HERDR_INTEGRATION_ID=vim
" HERDR_INTEGRATION_VERSION=2

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
let s:recovery_timer = -1
let s:recovery_notice_shown = 0
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

function! s:StateRoot() abort
  if !empty($XDG_STATE_HOME)
    return $XDG_STATE_HOME . '/herdr/vim'
  elseif has('win32') && !empty($LOCALAPPDATA)
    return $LOCALAPPDATA . '/herdr/vim'
  endif
  return expand('~/.local/state/herdr/vim')
endfunction

function! s:PrivateDirectory(path) abort
  call mkdir(a:path, 'p', 0700)
  return getftype(a:path) ==# 'dir' && (has('win32') || setfperm(a:path, 'rwx------') == 1)
endfunction

function! s:ProbeOutput(result, channel, data, ...) abort
  let a:result.text .= type(a:data) == v:t_list ? join(a:data, "\n") : a:data
endfunction

function! s:ProbeExit(result, job, code, ...) abort
  let a:result.code = a:code
endfunction

function! s:ProbeClosed(result, channel) abort
  let a:result.closed = 1
endfunction

function! s:RunHelper(binary, arguments) abort
  let args = [a:binary] + a:arguments
  let result = {'text': '', 'code': -1, 'closed': 0}
  if has('nvim')
    let job = jobstart(args, {'stdout_buffered': 1, 'on_stdout': function('s:ProbeOutput', [result])})
    if job <= 0
      return 'unknown'
    endif
    call chanclose(job, 'stdin')
    let status = jobwait([job], 1000)[0]
    if status != 0
      if status == -1
        silent! call jobstop(job)
      endif
      return 'unknown'
    endif
    return trim(result.text)
  endif
  let job = job_start(args, {'in_io': 'null', 'out_io': 'pipe', 'err_io': 'null', 'out_cb': function('s:ProbeOutput', [result]), 'exit_cb': function('s:ProbeExit', [result]), 'close_cb': function('s:ProbeClosed', [result])})
  if job_status(job) ==# 'fail'
    return 'unknown'
  endif
  let started = reltime()
  while (!result.closed || result.code < 0) && reltimefloat(reltime(started)) < 1.0
    sleep 10m
  endwhile
  if !result.closed || result.code < 0
    silent! call job_stop(job, 'kill')
    return 'unknown'
  endif
  return result.code == 0 ? trim(result.text) : 'unknown'
endfunction

function! s:StaleSwap(info) abort
  let user = empty($USER) ? $USERNAME : $USER
  if empty(user) || has_key(a:info, 'error') || get(a:info, 'user', '') !=# user || get(a:info, 'host', '') !=# hostname()
    return 0
  endif
  let pid = get(a:info, 'pid', -1)
  if pid == 0
    return 1
  elseif pid < 0
    return 0
  endif
  for binary in [$HERDR_BIN_PATH, exepath('herdr')]
    if empty(binary) || !executable(binary)
      continue
    endif
    let status = s:RunHelper(binary, ['integration', 'vim-process-status', string(pid)])
    if status ==# 'running'
      return 0
    elseif status ==# 'exited'
      return 1
    endif
  endfor
  return 0
endfunction

function! s:CommitSwap(original, archive, pid) abort
  for binary in [$HERDR_BIN_PATH, exepath('herdr')]
    if empty(binary) || !executable(binary)
      continue
    endif
    let status = s:RunHelper(binary, ['integration', 'vim-swap-commit', a:original, a:archive, string(a:pid)])
    if index(['archived', 'changed', 'busy'], status) >= 0
      return status
    endif
  endfor
  return 'unknown'
endfunction

function! s:RecoveryNotice(timer) abort
  let s:recovery_timer = -1
  if s:closing || s:recovery_notice_shown
    return
  endif
  let s:recovery_notice_shown = 1
  echohl WarningMsg
  echomsg strcharpart('Herdr: recovery saved; :HerdrRecoveries', 0, max([1, &columns - 1]))
  echohl None
endfunction

function! s:ArchiveSwap() abort
  if has('win32') || !empty(v:swapchoice) || !exists('*swapinfo') || getftype(v:swapname) !=# 'file' || filewritable(v:swapname) != 1
    return
  endif
  try
    let original = v:swapname
    let info = swapinfo(original)
    if !s:StaleSwap(info)
      return
    endif
    let root = s:StateRoot()
    let recovery = root . '/recovery'
    if !s:PrivateDirectory(root) || !s:PrivateDirectory(recovery)
      return
    endif
    let identity = strpart(sha256(original . ':' . getpid() . ':' . reltimestr(reltime())), 0, 32)
    let directory = recovery . '/' . strftime('%Y%m%d-%H%M%S') . '-' . identity
    if mkdir(directory, '', 0700) != 1
      return
    endif
    let archive = directory . '/recovery.swp'
    let contents = readfile(original, 'b')
    if writefile(contents, archive, 'bs') != 0 || (!has('win32') && setfperm(archive, 'rw-------') != 1)
      return
    endif
    let record = {'file': expand('<afile>:p'), 'swap': original, 'backup': archive, 'info': info, 'archived_at': localtime()}
    if writefile([json_encode(record)], directory . '/info.json', 's') != 0
      return
    endif
    if readfile(archive, 'b') !=# contents
      return
    endif
    if swapinfo(original) ==# info
      if !s:StaleSwap(info) || s:CommitSwap(original, archive, info.pid) ==# 'unknown'
        return
      endif
    endif
    let v:swapchoice = 'e'
    if get(info, 'dirty', 1) && !s:recovery_notice_shown && s:recovery_timer == -1
      let s:recovery_timer = timer_start(10, function('s:RecoveryNotice'))
    endif
  catch
    return
  endtry
endfunction

function! s:ListRecoveries() abort
  let lines = []
  for path in sort(globpath(s:StateRoot() . '/recovery', '*/info.json', 0, 1))
    try
      let record = json_decode(join(readfile(path), "\n"))
      call extend(lines, [record.file, '  ' . s:editor . ' -r ' . shellescape(record.backup), ''])
    catch
      call extend(lines, ['Unreadable recovery record: ' . path, ''])
    endtry
  endfor
  echo empty(lines) ? 'No Herdr swap recoveries.' : join(lines, "\n")
endfunction

function! s:Start() abort
  try
    let root = s:StateRoot()
    if !s:PrivateDirectory(root)
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

command! HerdrRecoveries call s:ListRecoveries()

augroup HerdrVim
  autocmd!
  autocmd SwapExists * call s:ArchiveSwap()
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
