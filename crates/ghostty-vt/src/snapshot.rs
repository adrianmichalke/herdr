use std::{ptr, slice};

use crate::{ffi, Error, GhosttyResultExt, Terminal};

struct EncodedSnapshot {
    ptr: *mut u8,
    len: usize,
}

impl Drop for EncodedSnapshot {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_free(ptr::null(), self.ptr, self.len) };
    }
}

struct SnapshotDecoder(ffi::GhosttySnapshotDecoder);

impl Drop for SnapshotDecoder {
    fn drop(&mut self) {
        unsafe { ffi::ghostty_snapshot_decoder_free(self.0) };
    }
}

impl Terminal {
    pub fn snapshot_cell_size(&self) -> (u32, u32) {
        let size = self.callback_state.size_report;
        (size.cell_width, size.cell_height)
    }

    pub fn restore_snapshot_cell_size(&mut self, cell_width_px: u32, cell_height_px: u32) {
        self.callback_state.size_report.cell_width = cell_width_px;
        self.callback_state.size_report.cell_height = cell_height_px;
    }

    pub fn kitty_graphics_enabled(&self) -> Result<bool, Error> {
        let mut storage_limit = 0u64;
        let storage_result = unsafe {
            ffi::ghostty_terminal_get(
                self.raw,
                ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_KITTY_IMAGE_STORAGE_LIMIT,
                (&mut storage_limit as *mut u64).cast(),
            )
        };
        if storage_result != ffi::GhosttyResult_GHOSTTY_NO_VALUE {
            storage_result.into_result()?;
        }
        Ok(storage_limit != 0)
    }

    pub fn encode_snapshot(&self) -> Result<Vec<u8>, Error> {
        if self.get_bool(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS_STATE)? {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }
        let mut encoded = EncodedSnapshot {
            ptr: ptr::null_mut(),
            len: 0,
        };
        unsafe {
            ffi::ghostty_snapshot_encode_alloc(
                self.raw,
                ptr::null(),
                &mut encoded.ptr,
                &mut encoded.len,
            )
            .into_result()?;
        }
        if encoded.ptr.is_null() || encoded.len == 0 {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }
        Ok(unsafe { slice::from_raw_parts(encoded.ptr, encoded.len) }.to_vec())
    }

    pub fn from_snapshot(snapshot: &[u8]) -> Result<Self, Error> {
        let mut decoder = SnapshotDecoder(ptr::null_mut());
        unsafe {
            ffi::ghostty_snapshot_decoder_new_buf(
                ptr::null(),
                &mut decoder.0,
                snapshot.as_ptr(),
                snapshot.len(),
            )
            .into_result()?;
        }
        let mut raw = ptr::null_mut();
        unsafe { ffi::ghostty_snapshot_decoder_decode(decoder.0, &mut raw).into_result()? };
        let mut terminal = Self::from_raw(raw, 0)?;
        let mut consumed = 0usize;
        unsafe {
            ffi::ghostty_snapshot_decoder_get(
                decoder.0,
                ffi::GhosttySnapshotDecoderData_GHOSTTY_SNAPSHOT_DECODER_DATA_SOURCE_OFFSET,
                (&mut consumed as *mut usize).cast(),
            )
            .into_result()?;
        }
        if consumed != snapshot.len() {
            return Err(Error(ffi::GhosttyResult_GHOSTTY_INVALID_VALUE));
        }
        terminal.max_scrollback = terminal
            .get_usize(ffi::GhosttyTerminalData_GHOSTTY_TERMINAL_DATA_SCROLLBACK_MAX_BYTES)?;
        terminal.configure_local_policies()?;
        Ok(terminal)
    }
}

#[cfg(test)]
mod tests;
