//! One saved position and style for dictation and meeting recording surfaces.
//! Coordinates remain in logical points, with a shared inset at every screen edge.
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

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum IndicatorStyle {
    #[default]
    Floating,
    Notch,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct IndicatorSettings {
    pub position: IndicatorPosition,
    pub style: IndicatorStyle,
}

impl IndicatorSettings {
    /// Position for the floating surface when no physical notch is used.
    /// Notch mode has a fixed fallback; never overwrite the saved Floating choice.
    pub fn floating_position(self) -> IndicatorPosition {
        match self.style {
            IndicatorStyle::Floating => self.position,
            IndicatorStyle::Notch => IndicatorPosition::BottomCenter,
        }
    }
}

pub fn current(app: &tauri::AppHandle) -> IndicatorSettings {
    app.state::<Mutex<IndicatorSettings>>()
        .lock()
        .map(|settings| *settings)
        .unwrap_or_default()
}

#[tauri::command]
pub fn set_indicator_settings(
    state: tauri::State<'_, Mutex<IndicatorSettings>>,
    settings: IndicatorSettings,
) -> Result<(), String> {
    *state
        .lock()
        .map_err(|_| "Recording indicator settings unavailable")? = settings;
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
    fn notch_fallback_is_bottom_center_without_changing_any_saved_floating_choice() {
        for position in [
            IndicatorPosition::TopLeft,
            IndicatorPosition::TopCenter,
            IndicatorPosition::TopRight,
            IndicatorPosition::BottomLeft,
            IndicatorPosition::BottomCenter,
            IndicatorPosition::BottomRight,
        ] {
            let mut settings = IndicatorSettings {
                position,
                style: IndicatorStyle::Notch,
            };
            let anchor = settings.floating_position();
            assert_eq!(anchor, IndicatorPosition::BottomCenter);
            // Both dictation and meeting controls retain 20pt bottom clearance.
            assert_eq!(anchor.offsets(1200.0, 800.0, 360.0, 40.0), (420.0, 20.0));
            assert_eq!(anchor.offsets(1200.0, 800.0, 320.0, 46.0), (440.0, 20.0));
            assert_eq!(settings.position, position);
            settings.style = IndicatorStyle::Floating;
            assert_eq!(settings.floating_position(), position);
        }
    }

    #[test]
    fn shared_settings_default_to_floating_bottom_center() {
        let settings = IndicatorSettings::default();
        assert_eq!(settings.position, IndicatorPosition::BottomCenter);
        assert_eq!(settings.style, IndicatorStyle::Floating);
    }

    #[test]
    fn settings_contract_contains_only_one_position_and_style() {
        let value = serde_json::json!({ "position": "top-right", "style": "notch" });
        let settings: IndicatorSettings = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(settings.position, IndicatorPosition::TopRight);
        assert_eq!(settings.style, IndicatorStyle::Notch);
        assert_eq!(serde_json::to_value(settings).unwrap(), value);
        assert!(
            serde_json::from_value::<IndicatorSettings>(serde_json::json!({
                "position": "top-right", "style": "notch", "meeting": "bottom-left"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<IndicatorSettings>(serde_json::json!({
                "dictation": "top-right", "meeting": "bottom-left",
                "dictationStyle": "notch", "meetingStyle": "floating"
            }))
            .is_err()
        );
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
