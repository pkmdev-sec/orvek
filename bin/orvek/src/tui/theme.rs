//! Configurable terminal colors and light/dark mode selection.

use crate::app::config::ReasoningEffort;
use orvek_harness::inference::Model;
use ratatui::style::Color;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use std::{fmt, str::FromStr, sync::mpsc::TryRecvError, thread, time::Duration};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ThemeMode {
    #[default]
    Auto,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ColorScheme {
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FeedbackTone {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub(crate) struct Theme {
    mode: ThemeMode,
    light: ThemePalette,
    dark: ThemePalette,
    #[serde(skip)]
    system_scheme: ColorScheme,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ThemePalette {
    background: ThemeColor,
    text: ThemeColor,
    border: ThemeColor,
    muted: ThemeColor,
    overlay_shadow: ThemeColor,
    accent: ThemeColor,
    brand_primary: ThemeColor,
    brand_secondary: ThemeColor,
    code_text: ThemeColor,
    code_background: ThemeColor,
    success: ThemeColor,
    warning: ThemeColor,
    error: ThemeColor,
    cancelled: ThemeColor,
    thinking_low: ThemeColor,
    thinking_medium: ThemeColor,
    thinking_high: ThemeColor,
    thinking_xhigh: ThemeColor,
    thinking_max: ThemeColor,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ThemeColor(Color);

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ThemeFields {
    mode: ThemeMode,
    light: PaletteFields,
    dark: PaletteFields,
    #[serde(flatten)]
    shared: PaletteFields,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PaletteFields {
    background: Option<ThemeColor>,
    text: Option<ThemeColor>,
    border: Option<ThemeColor>,
    muted: Option<ThemeColor>,
    overlay_shadow: Option<ThemeColor>,
    accent: Option<ThemeColor>,
    brand_primary: Option<ThemeColor>,
    brand_secondary: Option<ThemeColor>,
    code_text: Option<ThemeColor>,
    code_background: Option<ThemeColor>,
    success: Option<ThemeColor>,
    warning: Option<ThemeColor>,
    error: Option<ThemeColor>,
    cancelled: Option<ThemeColor>,
    thinking_low: Option<ThemeColor>,
    thinking_medium: Option<ThemeColor>,
    thinking_high: Option<ThemeColor>,
    thinking_xhigh: Option<ThemeColor>,
    thinking_max: Option<ThemeColor>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ColorValue {
    Name(String),
    Index(u8),
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            mode: ThemeMode::Auto,
            light: ThemePalette::light(),
            dark: ThemePalette::dark(),
            system_scheme: ColorScheme::Dark,
        }
    }
}

impl Theme {
    pub(crate) const fn mode(&self) -> ThemeMode {
        self.mode
    }

    pub(crate) const fn scheme(&self) -> ColorScheme {
        match self.mode {
            ThemeMode::Auto => self.system_scheme,
            ThemeMode::Light => ColorScheme::Light,
            ThemeMode::Dark => ColorScheme::Dark,
        }
    }

    pub(crate) fn set_mode(&mut self, mode: ThemeMode) {
        self.mode = mode;
    }

    pub(crate) fn set_system_scheme(&mut self, scheme: ColorScheme) -> bool {
        if self.system_scheme == scheme {
            return false;
        }
        self.system_scheme = scheme;
        self.mode == ThemeMode::Auto
    }

    pub(crate) fn replace_from_config(&mut self, mut theme: Self) {
        theme.system_scheme = self.system_scheme;
        *self = theme;
    }

    pub(crate) const fn background(&self) -> Color {
        self.palette().background.0
    }

    pub(crate) const fn text(&self) -> Color {
        self.palette().text.0
    }

    pub(crate) const fn border(&self) -> Color {
        self.palette().border.0
    }

    pub(crate) const fn muted(&self) -> Color {
        self.palette().muted.0
    }

    pub(crate) const fn overlay_shadow(&self) -> Color {
        self.palette().overlay_shadow.0
    }

    pub(crate) const fn accent(&self) -> Color {
        self.palette().accent.0
    }

    pub(crate) const fn brand_primary(&self) -> Color {
        self.palette().brand_primary.0
    }

    pub(crate) const fn brand_secondary(&self) -> Color {
        self.palette().brand_secondary.0
    }

    pub(crate) const fn code_text(&self) -> Color {
        self.palette().code_text.0
    }

    pub(crate) const fn code_background(&self) -> Color {
        self.palette().code_background.0
    }

    pub(crate) const fn success(&self) -> Color {
        self.palette().success.0
    }

    pub(crate) const fn warning(&self) -> Color {
        self.palette().warning.0
    }

    pub(crate) const fn error(&self) -> Color {
        self.palette().error.0
    }

    pub(crate) const fn cancelled(&self) -> Color {
        self.palette().cancelled.0
    }

    pub(crate) const fn feedback(&self, tone: FeedbackTone) -> Color {
        match tone {
            FeedbackTone::Info => self.accent(),
            FeedbackTone::Success => self.success(),
            FeedbackTone::Warning => self.warning(),
            FeedbackTone::Error => self.error(),
        }
    }

    pub(crate) const fn selection_background(&self) -> Color {
        self.code_background()
    }

    pub(crate) const fn scroll_track(&self) -> Color {
        self.border()
    }

    pub(crate) const fn scroll_thumb(&self) -> Color {
        self.accent()
    }

    pub(crate) const fn thinking_low(&self) -> Color {
        self.palette().thinking_low.0
    }

    pub(crate) const fn thinking_medium(&self) -> Color {
        self.palette().thinking_medium.0
    }

    pub(crate) const fn thinking_high(&self) -> Color {
        self.palette().thinking_high.0
    }

    pub(crate) const fn thinking_xhigh(&self) -> Color {
        self.palette().thinking_xhigh.0
    }

    pub(crate) const fn thinking_max(&self) -> Color {
        self.palette().thinking_max.0
    }

    pub(crate) const fn effort(&self, effort: ReasoningEffort) -> Color {
        match effort {
            ReasoningEffort::Low => self.thinking_low(),
            ReasoningEffort::Medium => self.thinking_medium(),
            ReasoningEffort::High => self.thinking_high(),
            ReasoningEffort::Xhigh => self.thinking_xhigh(),
            ReasoningEffort::Max => self.thinking_max(),
        }
    }

    pub(crate) const fn model(&self, model: Model) -> Color {
        match model {
            Model::Luna => self.text(),
            Model::Terra => self.success(),
            Model::Sol => self.warning(),
            Model::Glm => self.brand_secondary(),
            Model::Spark => self.brand_primary(),
            Model::Astra => self.accent(),
        }
    }

    const fn palette(&self) -> &ThemePalette {
        match self.scheme() {
            ColorScheme::Light => &self.light,
            ColorScheme::Dark => &self.dark,
        }
    }
}

impl ThemeMode {
    pub(crate) const ALL: [Self; 3] = [Self::Auto, Self::Light, Self::Dark];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }
}

impl ThemePalette {
    const fn dark() -> Self {
        Self {
            background: ThemeColor(Color::Rgb(0x12, 0x10, 0x18)),
            text: ThemeColor(Color::Rgb(0xF2, 0xE7, 0xD5)),
            border: ThemeColor(Color::Rgb(0x75, 0x6B, 0x78)),
            muted: ThemeColor(Color::Rgb(0x9E, 0x92, 0x91)),
            overlay_shadow: ThemeColor(Color::Rgb(0xD6, 0xB1, 0xAB)),
            accent: ThemeColor(Color::Rgb(0x82, 0xAE, 0xF5)),
            brand_primary: ThemeColor(Color::Rgb(0xB6, 0xA1, 0xF2)),
            brand_secondary: ThemeColor(Color::Rgb(0x89, 0xC9, 0xE8)),
            code_text: ThemeColor(Color::Rgb(0xF2, 0xE7, 0xD5)),
            code_background: ThemeColor(Color::Rgb(0x21, 0x1D, 0x2A)),
            success: ThemeColor(Color::Rgb(0x8F, 0xB9, 0x96)),
            warning: ThemeColor(Color::Rgb(0xD9, 0xAC, 0x72)),
            error: ThemeColor(Color::Rgb(0xE1, 0x8A, 0x9A)),
            cancelled: ThemeColor(Color::Rgb(0xA7, 0x8B, 0xFA)),
            thinking_low: ThemeColor(Color::Rgb(0xC9, 0xB4, 0x9A)),
            thinking_medium: ThemeColor(Color::Rgb(0xB6, 0xA1, 0xF2)),
            thinking_high: ThemeColor(Color::Rgb(0xD9, 0xAC, 0x72)),
            thinking_xhigh: ThemeColor(Color::Rgb(0xE1, 0x8A, 0x9A)),
            thinking_max: ThemeColor(Color::Rgb(0x93, 0x74, 0xD8)),
        }
    }

    const fn light() -> Self {
        Self {
            background: ThemeColor(Color::Rgb(0xF2, 0xE7, 0xD5)),
            text: ThemeColor(Color::Rgb(0x21, 0x1D, 0x2A)),
            border: ThemeColor(Color::Rgb(0x9E, 0x92, 0x91)),
            muted: ThemeColor(Color::Rgb(0x75, 0x6B, 0x78)),
            overlay_shadow: ThemeColor(Color::Rgb(0xD6, 0xB1, 0xAB)),
            accent: ThemeColor(Color::Rgb(0x4F, 0x74, 0xC8)),
            brand_primary: ThemeColor(Color::Rgb(0x73, 0x54, 0xA8)),
            brand_secondary: ThemeColor(Color::Rgb(0x4F, 0x74, 0xC8)),
            code_text: ThemeColor(Color::Rgb(0x21, 0x1D, 0x2A)),
            code_background: ThemeColor(Color::Rgb(0xE3, 0xD4, 0xC1)),
            success: ThemeColor(Color::Rgb(0x52, 0x7A, 0x58)),
            warning: ThemeColor(Color::Rgb(0x9A, 0x67, 0x2F)),
            error: ThemeColor(Color::Rgb(0xB5, 0x4F, 0x68)),
            cancelled: ThemeColor(Color::Rgb(0x62, 0x46, 0xA5)),
            thinking_low: ThemeColor(Color::Rgb(0x8A, 0x6E, 0x54)),
            thinking_medium: ThemeColor(Color::Rgb(0x73, 0x54, 0xA8)),
            thinking_high: ThemeColor(Color::Rgb(0x9A, 0x67, 0x2F)),
            thinking_xhigh: ThemeColor(Color::Rgb(0xB5, 0x4F, 0x68)),
            thinking_max: ThemeColor(Color::Rgb(0x6E, 0x56, 0xB3)),
        }
    }

    fn apply(&mut self, fields: &PaletteFields) {
        self.background = fields.background.unwrap_or(self.background);
        self.text = fields.text.unwrap_or(self.text);
        self.border = fields.border.unwrap_or(self.border);
        self.muted = fields.muted.unwrap_or(self.muted);
        self.overlay_shadow = fields.overlay_shadow.unwrap_or(self.overlay_shadow);
        self.accent = fields.accent.unwrap_or(self.accent);
        self.brand_primary = fields.brand_primary.unwrap_or(self.brand_primary);
        self.brand_secondary = fields.brand_secondary.unwrap_or(self.brand_secondary);
        self.code_text = fields.code_text.unwrap_or(self.code_text);
        self.code_background = fields.code_background.unwrap_or(self.code_background);
        self.success = fields.success.unwrap_or(self.success);
        self.warning = fields.warning.unwrap_or(self.warning);
        self.error = fields.error.unwrap_or(self.error);
        self.cancelled = fields.cancelled.unwrap_or(self.cancelled);
        self.thinking_low = fields.thinking_low.unwrap_or(self.thinking_low);
        self.thinking_medium = fields.thinking_medium.unwrap_or(self.thinking_medium);
        self.thinking_high = fields.thinking_high.unwrap_or(self.thinking_high);
        self.thinking_xhigh = fields.thinking_xhigh.unwrap_or(self.thinking_xhigh);
        self.thinking_max = fields.thinking_max.unwrap_or(self.thinking_max);
    }
}

impl<'de> Deserialize<'de> for Theme {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = ThemeFields::deserialize(deserializer)?;
        let mut light = ThemePalette::light();
        light.apply(&fields.shared);
        light.apply(&fields.light);
        let mut dark = ThemePalette::dark();
        dark.apply(&fields.shared);
        dark.apply(&fields.dark);
        Ok(Self {
            mode: fields.mode,
            light,
            dark,
            system_scheme: ColorScheme::Dark,
        })
    }
}

impl<'de> Deserialize<'de> for ThemeColor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = ColorValue::deserialize(deserializer)?;
        match value {
            ColorValue::Name(value) => Color::from_str(&value)
                .map(Self)
                .map_err(|_| de::Error::custom(format!("invalid terminal color `{value}`"))),
            ColorValue::Index(value) => Ok(Self(Color::Indexed(value))),
        }
    }
}

impl Serialize for ThemeColor {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&ColorName(self.0).to_string())
    }
}

fn color_scheme(mode: dark_light::Mode) -> Option<ColorScheme> {
    match mode {
        dark_light::Mode::Light => Some(ColorScheme::Light),
        dark_light::Mode::Dark => Some(ColorScheme::Dark),
        dark_light::Mode::Unspecified => None,
    }
}

pub(crate) fn detect_system_scheme() -> Option<ColorScheme> {
    dark_light::detect().ok().and_then(color_scheme)
}

const SYSTEM_SCHEME_WATCH_INTERVAL: Duration = Duration::from_millis(100);
const SYSTEM_SCHEME_FALLBACK_INTERVAL: Duration = Duration::from_secs(1);

fn publish_system_scheme(
    updates: &watch::Sender<Option<ColorScheme>>,
    scheme: ColorScheme,
) -> bool {
    if updates.is_closed() {
        return false;
    }
    if *updates.borrow() == Some(scheme) {
        return true;
    }
    updates.send(Some(scheme)).is_ok()
}

pub(crate) fn watch_system_scheme(
    updates: watch::Sender<Option<ColorScheme>>,
    shutdown: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut watcher = dark_light::subscribe().ok();
        if let Some(scheme) = detect_system_scheme()
            && !publish_system_scheme(&updates, scheme)
        {
            return;
        }
        loop {
            if shutdown.is_cancelled() || updates.is_closed() {
                return;
            }
            let (detected, interval) = match watcher.as_ref().map(dark_light::Watcher::try_recv) {
                Some(Ok(mode)) => (color_scheme(mode), SYSTEM_SCHEME_WATCH_INTERVAL),
                Some(Err(TryRecvError::Empty)) => (None, SYSTEM_SCHEME_WATCH_INTERVAL),
                Some(Err(TryRecvError::Disconnected)) => {
                    watcher = None;
                    (detect_system_scheme(), SYSTEM_SCHEME_FALLBACK_INTERVAL)
                }
                None => (detect_system_scheme(), SYSTEM_SCHEME_FALLBACK_INTERVAL),
            };
            if let Some(scheme) = detected
                && !publish_system_scheme(&updates, scheme)
            {
                return;
            }
            thread::sleep(interval);
        }
    })
}

struct ColorName(Color);

impl fmt::Display for ColorName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Color::Reset => formatter.write_str("reset"),
            Color::Black => formatter.write_str("black"),
            Color::Red => formatter.write_str("red"),
            Color::Green => formatter.write_str("green"),
            Color::Yellow => formatter.write_str("yellow"),
            Color::Blue => formatter.write_str("blue"),
            Color::Magenta => formatter.write_str("magenta"),
            Color::Cyan => formatter.write_str("cyan"),
            Color::Gray => formatter.write_str("gray"),
            Color::DarkGray => formatter.write_str("dark-gray"),
            Color::LightRed => formatter.write_str("light-red"),
            Color::LightGreen => formatter.write_str("light-green"),
            Color::LightYellow => formatter.write_str("light-yellow"),
            Color::LightBlue => formatter.write_str("light-blue"),
            Color::LightMagenta => formatter.write_str("light-magenta"),
            Color::LightCyan => formatter.write_str("light-cyan"),
            Color::White => formatter.write_str("white"),
            Color::Rgb(red, green, blue) => write!(formatter, "#{red:02X}{green:02X}{blue:02X}"),
            Color::Indexed(index) => write!(formatter, "{index}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ColorScheme, SYSTEM_SCHEME_WATCH_INTERVAL, Theme, ThemeMode, publish_system_scheme,
    };
    use orvek_harness::inference::Model;
    use ratatui::style::Color;

    #[test]
    fn auto_defaults_to_the_dark_palette_until_the_os_mode_is_known() {
        let theme = Theme::default();

        assert_eq!(theme.mode(), ThemeMode::Auto);
        assert_eq!(theme.background(), Color::Rgb(0x12, 0x10, 0x18));
        assert_eq!(theme.text(), Color::Rgb(0xF2, 0xE7, 0xD5));
        assert_eq!(theme.code_text(), Color::Rgb(0xF2, 0xE7, 0xD5));
        assert_eq!(theme.code_background(), Color::Rgb(0x21, 0x1D, 0x2A));
        assert_eq!(theme.thinking_medium(), Color::Rgb(0xB6, 0xA1, 0xF2));
        assert_eq!(theme.success(), Color::Rgb(0x8F, 0xB9, 0x96));
        assert_eq!(theme.warning(), Color::Rgb(0xD9, 0xAC, 0x72));
        assert_eq!(theme.error(), Color::Rgb(0xE1, 0x8A, 0x9A));
        assert_eq!(theme.cancelled(), Color::Rgb(0xA7, 0x8B, 0xFA));
    }

    #[test]
    fn system_theme_subscription_is_perceptually_immediate() {
        assert!(SYSTEM_SCHEME_WATCH_INTERVAL <= std::time::Duration::from_millis(100));
    }

    #[test]
    fn system_theme_updates_keep_only_the_latest_distinct_scheme() {
        let (updates, mut schemes) = tokio::sync::watch::channel(None);

        assert!(publish_system_scheme(&updates, ColorScheme::Dark));
        assert_eq!(*schemes.borrow_and_update(), Some(ColorScheme::Dark));
        assert!(!schemes.has_changed().unwrap());

        assert!(publish_system_scheme(&updates, ColorScheme::Dark));
        assert!(!schemes.has_changed().unwrap());

        assert!(publish_system_scheme(&updates, ColorScheme::Light));
        assert_eq!(*schemes.borrow_and_update(), Some(ColorScheme::Light));
        drop(schemes);
        assert!(!publish_system_scheme(&updates, ColorScheme::Dark));
    }

    #[test]
    fn models_have_a_shared_semantic_palette() {
        let theme = Theme::default();

        assert_eq!(theme.model(Model::Luna), theme.text());
        assert_eq!(theme.model(Model::Terra), theme.success());
        assert_eq!(theme.model(Model::Sol), theme.warning());
        assert_eq!(theme.model(Model::Glm), theme.brand_secondary());
        assert_eq!(theme.model(Model::Spark), theme.brand_primary());
        assert_eq!(theme.model(Model::Astra), theme.accent());
    }

    #[test]
    fn auto_tracks_the_system_scheme_while_explicit_modes_do_not() {
        let mut theme = Theme::default();

        assert!(theme.set_system_scheme(ColorScheme::Light));
        assert_eq!(theme.code_text(), Color::Rgb(0x21, 0x1D, 0x2A));
        assert_eq!(theme.code_background(), Color::Rgb(0xE3, 0xD4, 0xC1));

        theme.set_mode(ThemeMode::Dark);
        assert!(theme.set_system_scheme(ColorScheme::Dark).eq(&false));
        assert_eq!(theme.code_background(), Color::Rgb(0x21, 0x1D, 0x2A));
    }

    #[test]
    fn shared_and_mode_specific_colors_are_supported() {
        let mut theme: Theme = toml::from_str(
            "mode = \"light\"\nbackground = \"#FAF0E6\"\naccent = \"#12ABef\"\n[light]\nborder = 238\n[dark]\nborder = 239\n",
        )
        .unwrap();

        assert_eq!(theme.mode(), ThemeMode::Light);
        assert_eq!(theme.background(), Color::Rgb(0xFA, 0xF0, 0xE6));
        assert_eq!(theme.accent(), Color::Rgb(0x12, 0xAB, 0xEF));
        assert_eq!(theme.border(), Color::Indexed(238));
        theme.set_mode(ThemeMode::Dark);
        assert_eq!(theme.accent(), Color::Rgb(0x12, 0xAB, 0xEF));
        assert_eq!(theme.border(), Color::Indexed(239));
    }

    #[test]
    fn palettes_serialize_to_ratatui_compatible_strings() {
        let rendered = toml::to_string(&Theme::default()).unwrap();

        assert!(rendered.contains("mode = \"auto\""));
        assert!(rendered.contains("code_text = \"#F2E7D5\""));
        assert!(rendered.contains("code_background = \"#E3D4C1\""));
        assert!(rendered.contains("success = \"#527A58\""));
        assert!(rendered.contains("error = \"#B54F68\""));
        assert!(rendered.contains("thinking_max = \"#6E56B3\""));
    }

    #[test]
    fn overlay_shadow_has_its_own_theme_color_and_round_trips() {
        let mut theme: Theme =
            toml::from_str("overlay_shadow = \"#CBA29D\"\n[light]\noverlay_shadow = \"#E2C5BF\"\n")
                .unwrap();
        assert_eq!(theme.overlay_shadow(), Color::Rgb(0xCB, 0xA2, 0x9D));
        assert_eq!(theme.muted(), Theme::default().muted());
        theme.set_mode(ThemeMode::Light);
        assert_eq!(theme.overlay_shadow(), Color::Rgb(0xE2, 0xC5, 0xBF));
        let restored: Theme = toml::from_str(&toml::to_string(&theme).unwrap()).unwrap();
        assert_eq!(restored, theme);
    }

    #[test]
    fn invalid_colors_are_rejected() {
        let error = toml::from_str::<Theme>("text = \"ultraviolet\"").unwrap_err();

        assert!(error.to_string().contains("invalid terminal color"));
    }
}
