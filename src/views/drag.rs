#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DragKind {
    Move,
    ResizeStart,
    ResizeEnd,
}

impl DragKind {
    pub(crate) fn as_prefix(self) -> &'static str {
        match self {
            Self::Move => "move",
            Self::ResizeStart => "resize-start",
            Self::ResizeEnd => "resize-end",
        }
    }

    pub(crate) fn from_prefix(prefix: &str) -> Option<Self> {
        match prefix {
            "move" => Some(Self::Move),
            "resize-start" => Some(Self::ResizeStart),
            "resize-end" => Some(Self::ResizeEnd),
            _ => None,
        }
    }
}

pub(crate) fn drag_payload(kind: DragKind, event_id: i64) -> String {
    format!("{}:{event_id}", kind.as_prefix())
}

pub(crate) fn parse_drag_payload(value: &str) -> Option<(DragKind, i64)> {
    let (kind, event_id) = value.split_once(':')?;
    Some((DragKind::from_prefix(kind)?, event_id.parse().ok()?))
}
