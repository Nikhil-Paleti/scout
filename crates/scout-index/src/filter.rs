//! Content-based skip rules: size, binary, minified or generated.

pub const DEFAULT_MAX_FILE_SIZE: u64 = 1 << 20;
const BINARY_SNIFF: usize = 8 * 1024;
const MINIFIED_MIN_SIZE: usize = 1024;
const MINIFIED_AVG_LINE: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    TooLarge,
    Binary,
    Minified,
}

impl Skip {
    pub fn describe(self, max: u64) -> String {
        match self {
            Skip::TooLarge => format!("larger than {} bytes", max),
            Skip::Binary => "binary (NUL byte in the first 8 KB)".to_string(),
            Skip::Minified => "minified or generated (average line over 500 bytes)".to_string(),
        }
    }
}

pub fn check_size(size: u64, max: u64) -> Option<Skip> {
    if size > max {
        Some(Skip::TooLarge)
    } else {
        None
    }
}

pub fn check_content(data: &[u8]) -> Option<Skip> {
    let sniff = &data[..data.len().min(BINARY_SNIFF)];
    if memchr0(sniff) {
        return Some(Skip::Binary);
    }
    if data.len() >= MINIFIED_MIN_SIZE {
        let lines = data.iter().filter(|&&b| b == b'\n').count() + 1;
        if data.len() / lines > MINIFIED_AVG_LINE {
            return Some(Skip::Minified);
        }
    }
    None
}

fn memchr0(b: &[u8]) -> bool {
    b.contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules() {
        assert_eq!(check_content(b"hello\nworld\n"), None);
        assert_eq!(check_content(b"ab\0cd"), Some(Skip::Binary));
        let long = vec![b'x'; 4000];
        assert_eq!(check_content(&long), Some(Skip::Minified));
        let short_line = vec![b'x'; 600];
        assert_eq!(
            check_content(&short_line),
            None,
            "small files are never minified"
        );
        assert_eq!(
            check_size(2 << 20, DEFAULT_MAX_FILE_SIZE),
            Some(Skip::TooLarge)
        );
    }
}
