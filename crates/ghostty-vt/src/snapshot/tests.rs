use super::*;
use crate::{ActiveScreen, RenderCursor, RenderState};

fn text(terminal: &Terminal) -> String {
    terminal
        .read_text_screen(
            (0, 0),
            (
                terminal.cols().unwrap() - 1,
                terminal.total_rows().unwrap() as u32 - 1,
            ),
            false,
        )
        .unwrap()
}

fn cursor(terminal: &Terminal) -> RenderCursor {
    let mut render = RenderState::new().unwrap();
    render.update(terminal).unwrap();
    render.cursor().unwrap()
}

fn assert_same(left: &Terminal, right: &Terminal) {
    assert_eq!(
        left.active_screen().unwrap(),
        right.active_screen().unwrap()
    );
    assert_eq!(left.total_rows().unwrap(), right.total_rows().unwrap());
    assert_eq!(
        left.scrollback_rows().unwrap(),
        right.scrollback_rows().unwrap()
    );
    assert_eq!(text(left), text(right));
    assert_eq!(cursor(left), cursor(right));
    assert_eq!(
        left.kitty_keyboard_flags().unwrap(),
        right.kitty_keyboard_flags().unwrap()
    );
    assert_eq!(
        left.default_palette().unwrap(),
        right.default_palette().unwrap()
    );
    let bounds = (
        left.cols().unwrap() - 1,
        left.total_rows().unwrap() as u32 - 1,
    );
    assert_eq!(
        left.read_ansi_screen((0, 0), bounds, false, true).unwrap(),
        right.read_ansi_screen((0, 0), bounds, false, true).unwrap(),
    );
}

#[test]
fn snapshot_roundtrip_preserves_large_history_and_both_screen_cursors() {
    let mut source = Terminal::new(80, 6, 20_000_000).unwrap();
    source.resize(80, 6, 9, 18).unwrap();
    for line in 0..500 {
        source.write(format!("{line:04} history {}\r\n", "界é🙂".repeat(6)).as_bytes());
    }
    assert!(text(&source).len() > 8192);
    assert!(text(&source).contains("0000 history"));
    source.write(b"\x1b[2;4H\x1b[31mprimary\x1b7\x1b[4;9H\x1b[>1u\x1b[>5u");
    let primary_text = text(&source);
    let primary_cursor = cursor(&source);
    source.write(b"\x1b[?1049h\x1b[2;3HALTERNATE\x1b7\x1b[4;7H\x1b[4 q\x1b[?25l\x1b[>9u");
    let snapshot = source.encode_snapshot().unwrap();
    assert!(snapshot.len() > 8192);
    let mut restored = Terminal::from_snapshot(&snapshot).unwrap();
    let second = Terminal::from_snapshot(&restored.encode_snapshot().unwrap()).unwrap();
    assert_same(&restored, &second);
    assert_eq!(restored.max_scrollback(), source.max_scrollback());
    assert_eq!(restored.width_px().unwrap(), 720);
    assert_eq!(restored.height_px().unwrap(), 108);
    assert_eq!(restored.active_screen().unwrap(), ActiveScreen::Alternate);
    assert_same(&source, &restored);

    for bytes in [b"\x1b8!".as_slice(), b"\x1b[<u", b"\x1b[?25h\x1b[?1049l"] {
        source.write(bytes);
        restored.write(bytes);
        assert_same(&source, &restored);
    }
    assert_eq!(text(&restored), primary_text);
    assert_eq!(cursor(&restored), primary_cursor);
    for bytes in [
        b"\x1b[<u".as_slice(),
        b"\x1b8saved",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
    ] {
        source.write(bytes);
        restored.write(bytes);
        assert_same(&source, &restored);
    }
}

#[test]
fn snapshot_rejection_preserves_unfinished_input_without_enabling_tracking() {
    for (unfinished, continuation) in [
        (b"\x1b[31".as_slice(), b"mred".as_slice()),
        (b"\x1b]2;title".as_slice(), b"\x07after".as_slice()),
        (b"\xf0\x9f".as_slice(), b"\x99\x82".as_slice()),
    ] {
        let mut source = Terminal::new(40, 4, 4096).unwrap();
        let mut expected = Terminal::new(40, 4, 4096).unwrap();
        source.write(b"intact ");
        expected.write(b"intact ");
        source.write(unfinished);
        expected.write(unfinished);
        let before = text(&source);
        assert!(source.encode_snapshot().is_err());
        assert_eq!(text(&source), before);
        assert_eq!(
            source
                .get_usize(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_CONTINUATION_MAX_BYTES)
                .unwrap(),
            0
        );
        source.write(continuation);
        expected.write(continuation);
        assert_same(&source, &expected);
        let restored = Terminal::from_snapshot(&source.encode_snapshot().unwrap()).unwrap();
        assert_same(&source, &restored);
    }
}

#[test]
fn snapshot_roundtrip_restores_preexisting_parser_continuation() {
    let mut source = Terminal::new(40, 4, 4096).unwrap();
    let limit = 1024usize;
    unsafe {
        ffi::ghostty_terminal_set(
            source.raw,
            ffi::GhosttyTerminalOption_GHOSTTY_TERMINAL_OPT_CONTINUATION_MAX_BYTES,
            (&limit as *const usize).cast(),
        )
        .into_result()
        .unwrap();
    }
    source.write(b"before\x1b[38;2;123;");
    let mut restored = Terminal::from_snapshot(&source.encode_snapshot().unwrap()).unwrap();
    source.write(b"45;67mcontinued");
    restored.write(b"45;67mcontinued");
    assert_same(&source, &restored);
}

#[test]
fn snapshot_decoder_rejects_corrupt_truncated_and_trailing_data() {
    let mut source = Terminal::new(40, 4, 4096).unwrap();
    source.write(b"source survives rejected imports");
    let snapshot = source.encode_snapshot().unwrap();
    for len in [0, 1, snapshot.len() / 2, snapshot.len() - 1] {
        assert!(Terminal::from_snapshot(&snapshot[..len]).is_err());
    }
    let mut corrupt = snapshot.clone();
    let middle = corrupt.len() / 2;
    corrupt[middle] ^= 0x80;
    assert!(Terminal::from_snapshot(&corrupt).is_err());
    let mut trailing = snapshot.clone();
    trailing.push(0);
    assert!(Terminal::from_snapshot(&trailing).is_err());
    assert_eq!(source.encode_snapshot().unwrap(), snapshot);
}

#[test]
fn snapshot_accepts_enabled_but_unused_graphics() {
    let mut source = Terminal::new(40, 4, 4096).unwrap();
    source.enable_kitty_graphics().unwrap();
    source.write(b"unchanged");
    assert!(source.kitty_graphics_enabled().unwrap());
    let restored = Terminal::from_snapshot(&source.encode_snapshot().unwrap()).unwrap();
    assert_same(&source, &restored);
    source.write(b"\x1b[?1049hALTERNATE");
    let restored = Terminal::from_snapshot(&source.encode_snapshot().unwrap()).unwrap();
    assert_same(&source, &restored);
}

#[test]
fn snapshot_rejects_graphics_state_on_either_screen() {
    for alternate in [false, true] {
        let mut source = Terminal::new(40, 4, 4096).unwrap();
        source.enable_kitty_graphics().unwrap();
        source.write(b"primary");
        if alternate {
            source.write(b"\x1b[?1049hALTERNATE");
        }
        source.write(b"\x1b_Ga=t,f=24,s=1,v=1,i=31;AAAA\x1b\\");
        let before = text(&source);
        assert!(source.encode_snapshot().is_err());
        assert_eq!(text(&source), before);
        source.write(if alternate {
            b"\x1b[?1049l"
        } else {
            b"\x1b[?1049h"
        });
        assert!(
            source.encode_snapshot().is_err(),
            "hidden screen graphics must not be lost"
        );
    }
}

#[test]
fn snapshot_restores_callbacks_without_replaying_side_effects() {
    let mut source = Terminal::new(40, 4, 4096).unwrap();
    source.resize(40, 4, 9, 18).unwrap();
    source.write(b"\x07\x1b]52;c;c2VjcmV0\x07\x1b[3;7H");
    let mut restored = Terminal::from_snapshot(&source.encode_snapshot().unwrap()).unwrap();
    assert_eq!(restored.take_bell_count(), 0);
    assert!(restored.take_clipboard_writes().is_empty());
    let replies = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let callback_replies = replies.clone();
    restored
        .set_write_pty_callback(move |bytes| {
            callback_replies.lock().unwrap().extend_from_slice(bytes)
        })
        .unwrap();
    restored.write(b"\x1b[6n\x1b[18t\x1b[16t");
    let replies = replies.lock().unwrap();
    let replies = String::from_utf8_lossy(&replies);
    assert!(replies.contains("\x1b[3;7R"));
    assert!(replies.contains("\x1b[8;4;40t"));
    assert!(replies.contains("\x1b[6;18;9t"));
}
