use colored::Colorize;

pub const BLUE:  (u8, u8, u8)   = (0x9d, 0xac, 0xff); // 9dacff
pub const PINK:  (u8, u8, u8)   = (0xff, 0xd0, 0xd7); // ffd0d7
pub const WHITE: (u8, u8, u8)   = (0xe4, 0xe4, 0xe4); // e4e4e4
pub const DARK:  (u8, u8, u8)   = (0x08, 0x08, 0x08); // 080808
pub const GREEN: (u8, u8, u8)   = (0x70, 0xe3, 0x2b); // 70e32b

pub fn blue(s: impl std::fmt::Display) -> colored::ColoredString {
    format!("{}", s).truecolor(BLUE.0, BLUE.1, BLUE.2)
}

pub fn pink(s: impl std::fmt::Display) -> colored::ColoredString {
    format!("{}", s).truecolor(PINK.0, PINK.1, PINK.2)
}

pub fn white(s: impl std::fmt::Display) -> colored::ColoredString {
    format!("{}", s).truecolor(WHITE.0, WHITE.1, WHITE.2)
}

pub fn dark(s: impl std::fmt::Display) -> colored::ColoredString {
    format!("{}", s).truecolor(DARK.0, DARK.1, DARK.2)
}

pub fn green(s: impl std::fmt::Display) -> colored::ColoredString {
    format!("{}", s).truecolor(GREEN.0, GREEN.1, GREEN.2)
}
