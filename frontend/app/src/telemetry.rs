use super::userid::get_userid;
use crate::{js, userid::get_username};
use log::warn;
use oort_telemetry_proto::{Telemetry, TelemetryMsg};

pub fn send(payload: Telemetry) {
    let userid = get_userid();
    let username = get_username();
    let msg = TelemetryMsg {
        payload,
        build: crate::version(),
        userid,
        username: Some(username),
    };
    match serde_json::to_string(&msg) {
        Ok(serialized) => js::telemetry::send_telemetry(&serialized),
        Err(msg) => warn!("Failed to serialize telemetry: {}", msg),
    };
}