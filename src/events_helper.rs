use kube::runtime::events::{Event, EventType, Recorder};
use kube::{Resource, ResourceExt};

/// Publish a simple Normal event with given reason and note, ignoring errors.
pub async fn emit_info<R: Resource<DynamicType = ()> + ResourceExt>(
    recorder: &Recorder,
    obj: &R,
    reason: &str,
    action: &str,
    note: impl Into<Option<String>>,
) {
    let _ = recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: reason.into(),
                note: note.into(),
                action: action.into(),
                secondary: None,
            },
            &obj.object_ref(&()),
        )
        .await;
}
