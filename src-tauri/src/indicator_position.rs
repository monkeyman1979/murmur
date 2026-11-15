//! Saved anchors for the two recording windows. Coordinates remain in logical
//! points, with the existing dictation inset shared by every screen edge.
use serde::{Deserialize, Serialize};
use std::sync::Mutex;
use tauri::Manager;

const EDGE_INSET: f64 = 20.0;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum IndicatorPosition {
    TopLeft,
    TopCenter,
    TopRight,
    BottomLeft,
    #[default]
    BottomCenter,
    BottomRight,
}

impl IndicatorPosition {
    /// Window origin relative to the usable screen's bottom-left corner
    /// (AppKit coordinates). Clamp for screens smaller than the window.
    pub fn offsets(
        self,
        width: f64,
        height: f64,
        window_width: f64,
        window_height: f64,
    ) -> (f64, f64) {
        let free_x = (width - window_width).max(0.0);
        let free_y = (height - window_height).max(0.0);
        let inset_x = EDGE_INSET.min(free_x / 2.0);
        let inset_y = EDGE_INSET.min(free_y / 2.0);
        let x = match self {
            Self::TopLeft | Self::BottomLeft => inset_x,
            Self::TopCenter | Self::BottomCenter => free_x / 2.0,
            Self::TopRight | Self::BottomRight => free_x - inset_x,
        };
        let y = match self {
            Self::TopLeft | Self::TopCenter | Self::TopRight => free_y - inset_y,
            _ => inset_y,
        };
        (x, y)
    }
}

#[derive(Clone, Copy, Default)]
pub struct IndicatorPositions {
    pub dictation: IndicatorPosition,
    pub meeting: IndicatorPosition,
}

pub fn current(app: &tauri::AppHandle) -> IndicatorPositions {
    app.state::<Mutex<IndicatorPositions>>()
        .lock()
        .map(|positions| *positions)
        .unwrap_or_default()
}

#[tauri::command]
pub fn set_indicator_positions(
    state: tauri::State<'_, Mutex<IndicatorPositions>>,
    dictation: IndicatorPosition,
    meeting: IndicatorPosition,
) -> Result<(), String> {
    *state
        .lock()
        .map_err(|_| "Recording position state unavailable")? =
        IndicatorPositions { dictation, meeting };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_six_anchors_use_the_existing_twenty_point_inset() {
        for (position, expected) in [
            (IndicatorPosition::TopLeft, (20.0, 740.0)),
            (IndicatorPosition::TopCenter, (420.0, 740.0)),
            (IndicatorPosition::TopRight, (820.0, 740.0)),
            (IndicatorPosition::BottomLeft, (20.0, 20.0)),
            (IndicatorPosition::BottomCenter, (420.0, 20.0)),
            (IndicatorPosition::BottomRight, (820.0, 20.0)),
        ] {
            assert_eq!(position.offsets(1200.0, 800.0, 360.0, 40.0), expected);
        }
    }

    #[test]
    fn both_windows_default_to_bottom_center() {
        let positions = IndicatorPositions::default();
        assert_eq!(positions.dictation, IndicatorPosition::BottomCenter);
        assert_eq!(positions.meeting, IndicatorPosition::BottomCenter);
    }

    #[test]
    fn placement_uses_the_actual_window_size() {
        assert_eq!(
            IndicatorPosition::TopRight.offsets(1200.0, 800.0, 320.0, 46.0),
            (860.0, 734.0)
        );
    }

    #[test]
    fn small_work_areas_never_produce_negative_offsets() {
        assert_eq!(
            IndicatorPosition::TopRight.offsets(300.0, 30.0, 360.0, 40.0),
            (0.0, 0.0)
        );
        assert_eq!(
            IndicatorPosition::BottomLeft.offsets(380.0, 60.0, 360.0, 40.0),
            (10.0, 10.0)
        );
    }

    #[test]
    fn only_supported_anchors_are_accepted_over_ipc() {
        assert_eq!(
            serde_json::from_str::<IndicatorPosition>("\"top-right\"").unwrap(),
            IndicatorPosition::TopRight
        );
        assert!(serde_json::from_str::<IndicatorPosition>("\"center\"").is_err());
    }
}
