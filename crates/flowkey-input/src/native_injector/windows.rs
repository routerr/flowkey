use super::NativeInputSink;
use enigo::Mouse;

pub(super) fn move_mouse(sink: &mut NativeInputSink, dx: i32, dy: i32) -> Result<(), String> {
    sink.enigo
        .move_mouse(dx, dy, enigo::Coordinate::Rel)
        .map_err(|error| error.to_string())
}

pub(super) fn reset_state(_sink: &mut NativeInputSink) {}
