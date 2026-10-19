//! Детерминированные визуальные метаданные аватара.
//!
//! UI-клиенты не должны дублировать логику выбора цвета и инициалов: цвет
//! стабильно выводится из идентификатора сущности (FNV-1a 64), а инициалы —
//! из отображаемого имени с безопасным Unicode-разбором. Функции не паникуют
//! на пустых строках, эмодзи и спецсимволах.

/// Палитра из 8 сбалансированных цветов в стиле Telegram/Signal.
pub const AVATAR_PALETTE: [&str; 8] = [
    "#E17076", "#FAA774", "#A695E7", "#7BC862", "#6EC9CB", "#65AADD", "#EE7AAE", "#E5CA77",
];

/// Готовые метаданные аватара для JSON FFI и UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Avatar {
    /// 1–2 буквы в верхнем регистре; пустая строка, если букв нет.
    pub initials: String,
    /// Индекс в [`AVATAR_PALETTE`] (`0..=7`).
    pub color_index: u8,
    /// HEX-код цвета из палитры.
    pub color_hex: &'static str,
}

impl Avatar {
    /// Собирает аватар по идентификатору и имени.
    ///
    /// `fallback` используется, если в `display_name` нет ни одной буквы
    /// (пустая строка, только эмодзи или спецсимволы).
    #[must_use]
    pub fn new(id: &str, display_name: &str, fallback: &str) -> Self {
        let color_index = color_index(id);
        Self {
            initials: initials(display_name, fallback),
            color_index,
            color_hex: color_hex(color_index),
        }
    }
}

/// Стабильный индекс цвета `0..=7` по UTF-8 идентификатору.
///
/// Используется FNV-1a 64: реализация не зависит от версии Rust, платформы
/// или порядка хэширования в `HashMap`, поэтому все устройства видят один
/// и тот же цвет для одного `user_id`/`chat_id`.
#[must_use]
pub fn color_index(id: &str) -> u8 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (hash % AVATAR_PALETTE.len() as u64) as u8
}

/// HEX-код цвета по индексу. Неизвестный индекс безопасно даёт первый цвет.
#[must_use]
pub fn color_hex(index: u8) -> &'static str {
    AVATAR_PALETTE
        .get(usize::from(index))
        .copied()
        .unwrap_or(AVATAR_PALETTE[0])
}

/// Инициалы: до двух первых букв из `display_name`, иначе из `fallback`.
///
/// * два и более слова — первые буквы первых двух слов (`"Иван Петров"` →
///   `"ИП"`);
/// * одно слово — до двух первых букв (`"Алиса"` → `"АЛ"`);
/// * буквы берутся только из `char::is_alphabetic`, поэтому эмодзи, цифры и
///   пунктуация пропускаются, а кириллица, латиница и CJK работают
///   одинаково;
/// * если букв нет ни в имени, ни в фоллбэке — пустая строка.
#[must_use]
pub fn initials(display_name: &str, fallback: &str) -> String {
    let primary = initials_of(display_name);
    if !primary.is_empty() {
        return primary;
    }
    initials_of(fallback)
}

fn initials_of(name: &str) -> String {
    let words: Vec<Vec<char>> = name
        .split_whitespace()
        .map(|word| word.chars().filter(|ch| ch.is_alphabetic()).collect())
        .filter(|letters: &Vec<char>| !letters.is_empty())
        .collect();
    let picked: Vec<char> = match words.as_slice() {
        [] => Vec::new(),
        [word] => word.iter().take(2).copied().collect(),
        [first, second, ..] => vec![first[0], second[0]],
    };
    picked
        .into_iter()
        .flat_map(char::to_uppercase)
        .take(2)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_is_stable() {
        assert_eq!(AVATAR_PALETTE.len(), 8);
        assert_eq!(color_hex(0), "#E17076");
        assert_eq!(color_hex(1), "#FAA774");
        assert_eq!(color_hex(7), "#E5CA77");
        assert_eq!(color_hex(255), "#E17076");
    }

    #[test]
    fn color_index_is_deterministic() {
        assert_eq!(color_index("alice"), 7);
        assert_eq!(color_index("bob"), 4);
        assert_eq!(color_index("user-1"), 4);
        assert_eq!(color_index(""), 5);
        for id in ["alice", "bob", "user-1", "чат-42", "😀"] {
            assert_eq!(color_index(id), color_index(id));
            assert!(color_index(id) < 8);
        }
    }

    #[test]
    fn initials_of_two_words() {
        assert_eq!(initials("Иван Петров", ""), "ИП");
        assert_eq!(initials("Élodie Durand", ""), "ÉD");
        assert_eq!(initials("  Иван   Петров  ", ""), "ИП");
        assert_eq!(initials("日本語 テスト", ""), "日テ");
        assert_eq!(initials("Иван Петров Сидоров", ""), "ИП");
    }

    #[test]
    fn initials_of_one_word() {
        assert_eq!(initials("Алиса", ""), "АЛ");
        assert_eq!(initials("bob", ""), "BO");
        assert_eq!(initials("Я", ""), "Я");
        assert_eq!(initials("ßa", ""), "SS");
    }

    #[test]
    fn emoji_and_punctuation_do_not_panic() {
        assert_eq!(initials("😀", ""), "");
        assert_eq!(initials("😀 🚀 ✨", ""), "");
        assert_eq!(initials("👩‍💻 Alice", ""), "AL");
        assert_eq!(initials("!!! @@@ ###", ""), "");
        assert_eq!(initials("\u{301}\u{200d}", ""), "");
        assert_eq!(initials("12345", ""), "");
    }

    #[test]
    fn fallback_is_used_when_primary_has_no_letters() {
        assert_eq!(initials("", "alice"), "AL");
        assert_eq!(initials("😀", "bob"), "BO");
        assert_eq!(initials("   ", "Иван Петров"), "ИП");
        assert_eq!(initials("", ""), "");
        assert_eq!(initials("😀", "🚀"), "");
    }

    #[test]
    fn avatar_combines_id_and_name() {
        let avatar = Avatar::new("alice", "Alice", "alice");
        assert_eq!(
            avatar,
            Avatar {
                initials: "AL".to_string(),
                color_index: 7,
                color_hex: "#E5CA77",
            }
        );
        // Один id всегда даёт один цвет, даже при другом имени.
        assert_eq!(Avatar::new("alice", "Bob", "bob").color_hex, "#E5CA77");
    }
}
