pub(crate) mod journal;
pub(crate) mod routing;
#[cfg(unix)]
pub(crate) mod transport;

pub(crate) const MAX_FORWARD_HOPS: u8 = 8;
#[cfg(unix)]
pub(crate) const TRANSFER_PROTOCOL_VERSION: u32 = 1;

pub(crate) fn valid_transfer_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub(crate) fn session_name() -> String {
    crate::session::active_name().unwrap_or_else(|| crate::session::DEFAULT_SESSION_NAME.into())
}

pub(crate) fn error_response(id: String, code: &str, message: impl Into<String>) -> String {
    serde_json::to_string(&crate::api::schema::ErrorResponse {
        id,
        error: crate::api::schema::ErrorBody {
            code: code.into(),
            message: message.into(),
        },
    })
    .unwrap_or_else(|_| "{}".into())
}
