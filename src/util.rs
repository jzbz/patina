//! Small formatting and path helpers.

/// True for URLs and other non-file sources mpv understands.
pub fn is_url(source: &str) -> bool {
    match source.split_once("://") {
        Some((scheme, _)) => {
            !scheme.is_empty()
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        }
        None => source.starts_with("magnet:") || source.starts_with("data:"),
    }
}

/// True for URLs other than file:// ones: what comes over the network.
pub fn is_remote(source: &str) -> bool {
    is_url(source)
        && !source
            .get(..7)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
}

/// True for web pages: http:// and https:// URLs.
pub fn is_web(source: &str) -> bool {
    ["http://", "https://"].iter().any(|scheme| {
        source
            .get(..scheme.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(scheme))
    })
}

/// Seconds as `mm:ss` or `h:mm:ss`, matching the interface's clock.
pub fn format_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "00:00".into();
    }
    let total = seconds.floor() as u64;
    let (hours, minutes, secs) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{secs:02}")
    } else {
        format!("{minutes:02}:{secs:02}")
    }
}

/// A delay with sign and unit, e.g. "+0.25 s".
pub fn format_delay(seconds: f64) -> String {
    let rounded = (seconds * 100.0).round() / 100.0;
    if rounded == 0.0 {
        "0 s".into()
    } else {
        format!("{rounded:+} s")
    }
}

/// Speed as IINA shows it: "1.5×", "0.25×".
pub fn format_speed(speed: f64) -> String {
    let text = format!("{:.2}", speed);
    format!("{}×", text.trim_end_matches('0').trim_end_matches('.'))
}

/// Parses "1:23:45", "12:30", "90", "90.5" or "50%".
pub enum TimeSpec {
    Seconds(f64),
    Percent(f64),
}

pub fn parse_time(text: &str) -> Option<TimeSpec> {
    let text = text.trim();
    if let Some(percent) = text.strip_suffix('%') {
        let value: f64 = percent.trim().parse().ok()?;
        return (0.0..=100.0)
            .contains(&value)
            .then_some(TimeSpec::Percent(value));
    }
    let mut total = 0.0;
    let parts: Vec<&str> = text.split(':').collect();
    if parts.is_empty() || parts.len() > 3 {
        return None;
    }
    for (index, part) in parts.iter().enumerate() {
        let value: f64 = part.trim().parse().ok()?;
        if value < 0.0 || (index > 0 && value >= 60.0) {
            return None;
        }
        total = total * 60.0 + value;
    }
    Some(TimeSpec::Seconds(total))
}

/// The directory of a local path, for list subtitles.
pub fn parent_directory(path: &str) -> String {
    if is_url(path) {
        return path
            .split("://")
            .nth(1)
            .and_then(|rest| rest.split('/').next())
            .unwrap_or(path)
            .to_owned();
    }
    let path = std::path::Path::new(path);
    let parent = path
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();
    match directories::BaseDirs::new() {
        Some(dirs) => {
            let home = dirs.home_dir().to_string_lossy().into_owned();
            match parent.strip_prefix(&home) {
                Some(rest) if !home.is_empty() => format!("~{rest}"),
                _ => parent,
            }
        }
        None => parent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_recognized() {
        assert!(is_url("https://example.com/a.mp4"));
        assert!(is_url("bd://"));
        assert!(is_url("magnet:?xt=urn:btih:abc"));
        assert!(!is_url("/home/user/a.mkv"));
        assert!(!is_url("C:\\Videos\\a.mkv"));
        assert!(!is_url("://nope"));
    }

    #[test]
    fn times_format_like_the_clock() {
        assert_eq!(format_time(0.0), "00:00");
        assert_eq!(format_time(59.9), "00:59");
        assert_eq!(format_time(61.0), "01:01");
        assert_eq!(format_time(3725.0), "1:02:05");
        assert_eq!(format_time(f64::NAN), "00:00");
    }

    #[test]
    fn delays_and_speeds_format_compactly() {
        assert_eq!(format_delay(0.0), "0 s");
        assert_eq!(format_delay(0.25), "+0.25 s");
        assert_eq!(format_delay(-1.5), "-1.5 s");
        assert_eq!(format_speed(1.0), "1×");
        assert_eq!(format_speed(1.5), "1.5×");
        assert_eq!(format_speed(0.25), "0.25×");
    }

    #[test]
    fn jump_to_accepts_clock_times_and_percentages() {
        let seconds = |text| match parse_time(text) {
            Some(TimeSpec::Seconds(value)) => Some(value),
            _ => None,
        };
        assert_eq!(seconds("90"), Some(90.0));
        assert_eq!(seconds("1:30"), Some(90.0));
        assert_eq!(seconds("1:02:03"), Some(3723.0));
        assert_eq!(seconds("1:75"), None);
        assert_eq!(seconds("abc"), None);
        assert!(matches!(parse_time("50%"), Some(TimeSpec::Percent(50.0))));
        assert!(parse_time("150%").is_none());
    }
}
