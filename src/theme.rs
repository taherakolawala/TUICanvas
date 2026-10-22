//! Colours. UF orange and blue as accents, urgency colours for deadlines.
//! Setting NO_COLOR (https://no-color.org) keeps only bold/underline/reverse.

use std::sync::OnceLock;

use ratatui::style::{Color, Modifier, Style};

use crate::model::Tone;

pub const ORANGE: Color = Color::Rgb(250, 70, 22);
pub const BLUE: Color = Color::Rgb(0, 33, 165);
pub const SKY: Color = Color::Rgb(115, 190, 255);
pub const GREEN: Color = Color::Rgb(95, 205, 125);
pub const AMBER: Color = Color::Rgb(240, 195, 70);
pub const PEACH: Color = Color::Rgb(255, 145, 70);
pub const RED: Color = Color::Rgb(245, 90, 90);
pub const MUTED: Color = Color::Rgb(155, 155, 165);
pub const FAINT: Color = Color::Rgb(105, 105, 118);
pub const SELECT_BG: Color = Color::Rgb(34, 46, 82);

fn plain() -> bool {
    static PLAIN: OnceLock<bool> = OnceLock::new();
    *PLAIN.get_or_init(|| std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()))
}

fn fg(color: Color) -> Style {
    if plain() {
        Style::new()
    } else {
        Style::new().fg(color)
    }
}

fn badge(fg_color: Color, bg_color: Color) -> Style {
    if plain() {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().fg(fg_color).bg(bg_color)
    }
}

/// Foreground colour (course colours), or plain under NO_COLOR.
pub fn color(c: Color) -> Style {
    fg(c)
}
pub fn text() -> Style {
    Style::new()
}
pub fn bold() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}
pub fn accent() -> Style {
    fg(ORANGE)
}
pub fn heading() -> Style {
    fg(ORANGE).add_modifier(Modifier::BOLD)
}
pub fn muted() -> Style {
    fg(MUTED)
}
pub fn faint() -> Style {
    fg(FAINT)
}
pub fn code() -> Style {
    fg(SKY)
}
pub fn link() -> Style {
    fg(SKY).add_modifier(Modifier::UNDERLINED)
}
pub fn key() -> Style {
    fg(SKY).add_modifier(Modifier::BOLD)
}
pub fn favorite() -> Style {
    fg(AMBER)
}
pub fn bar() -> Style {
    if plain() {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().fg(Color::White).bg(BLUE)
    }
}
pub fn bar_dim() -> Style {
    if plain() {
        bar()
    } else {
        Style::new().fg(Color::Rgb(185, 195, 235)).bg(BLUE)
    }
}
pub fn bar_separator() -> Style {
    if plain() {
        bar()
    } else {
        Style::new().fg(ORANGE).bg(BLUE)
    }
}
pub fn syncing() -> Style {
    badge(Color::Black, AMBER)
}
pub fn updated() -> Style {
    badge(Color::Black, GREEN)
}
pub fn failed() -> Style {
    badge(Color::White, Color::Rgb(185, 40, 40)).add_modifier(Modifier::BOLD)
}
pub fn selected() -> Style {
    if plain() {
        Style::new().add_modifier(Modifier::REVERSED)
    } else {
        Style::new().bg(SELECT_BG).add_modifier(Modifier::BOLD)
    }
}
pub fn tab_active() -> Style {
    if plain() {
        Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::new()
            .fg(ORANGE)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    }
}
pub fn table_header() -> Style {
    fg(ORANGE).add_modifier(Modifier::BOLD)
}
pub fn notice() -> Style {
    fg(SKY)
}
pub fn alert() -> Style {
    badge(Color::Black, AMBER)
}
pub fn error() -> Style {
    fg(RED)
}

pub fn tone(tone: Tone) -> Style {
    match tone {
        Tone::Good => fg(GREEN),
        Tone::Info => fg(SKY),
        Tone::Warn => fg(AMBER),
        Tone::Bad => fg(RED).add_modifier(Modifier::BOLD),
        Tone::Neutral => fg(MUTED),
    }
}

/// Deadline colour: overdue work red, due within a day peach, within a week
/// amber, later plain. Finished work and missing dates recede.
pub fn due(
    due: Option<chrono::DateTime<chrono::Utc>>,
    done: bool,
    now: chrono::DateTime<chrono::Utc>,
) -> Style {
    let Some(due) = due else { return faint() };
    if done {
        return muted();
    }
    let left = due - now;
    if left < chrono::Duration::zero() {
        fg(RED).add_modifier(Modifier::BOLD)
    } else if left < chrono::Duration::hours(24) {
        fg(PEACH).add_modifier(Modifier::BOLD)
    } else if left < chrono::Duration::days(7) {
        fg(AMBER)
    } else {
        text()
    }
}
