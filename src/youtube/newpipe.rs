//! The file NewPipe exports its subscriptions to, read and written so that
//! channels can move between it and Patina: a JSON object with the app's
//! version and a `subscriptions` array of `service_id`, `url` and `name`.
//!
//! NewPipe follows more than YouTube, so an entry for another service is
//! counted and skipped. Only an address with the channel's id in it can be
//! used: an `@handle`, `/c/` or `/user/` address needs a request to turn into
//! one, and reading a file asks nobody. Those are skipped too.
//!
//! As with Google's files, nothing from the file is used without being
//! checked, the address is built again from the id, and entries past
//! `ROW_LIMIT` are only counted.

use super::store::Channel;
use super::takeout::{ImportError, Parsed, ROW_LIMIT, Rows};
use super::{ChannelInput, clean_text};
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use std::fmt;

/// The release whose numbers an export carries: 0.29.1 (version code 1015),
/// the newest when this was written. NewPipe writes its own there and reads
/// them back without checking, so any release would do, but a real one keeps
/// the file looking like one NewPipe made, and the file follows its order.
const APP_VERSION: &str = "0.29.1";
const APP_VERSION_INT: u32 = 1015;

/// NewPipe's number for YouTube. SoundCloud, media.ccc.de, PeerTube and
/// Bandcamp have others.
const YOUTUBE: u64 = 0;

/// Whether the file is JSON rather than CSV, by its first character. Neither
/// of Google's files can begin with a brace or a bracket.
pub fn is_json(text: &str) -> bool {
    text.trim_start().starts_with(['{', '['])
}

/// The entries kept, and how many more there were. A file's other fields are
/// ignored, as NewPipe's own version numbers are.
#[derive(Deserialize)]
struct Export {
    subscriptions: Entries,
}

struct Entries {
    kept: Vec<Entry>,
    more: usize,
}

impl<'de> Deserialize<'de> for Entries {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Counting;

        impl<'de> Visitor<'de> for Counting {
            type Value = Entries;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a list of subscriptions")
            }

            /// Past the limit an entry is only stepped over, and before it only
            /// its three fields are kept, so the memory is what those need
            /// however long the file is and whatever an entry holds besides.
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Entries, A::Error> {
                let mut entries = Entries {
                    kept: Vec::new(),
                    more: 0,
                };
                while entries.kept.len() < ROW_LIMIT {
                    match seq.next_element::<Entry>()? {
                        Some(entry) => entries.kept.push(entry),
                        None => return Ok(entries),
                    }
                }
                while seq.next_element::<IgnoredAny>()?.is_some() {
                    entries.more += 1;
                }
                Ok(entries)
            }
        }

        deserializer.deserialize_seq(Counting)
    }
}

/// One entry's three fields, each only if it has the type NewPipe gives it.
/// Anything else in the entry is stepped over without being kept, since a
/// wrong file could put most of its 32 MB in one field, and an entry that
/// isn't an object reads as one with nothing in it, to be skipped.
#[derive(Default)]
struct Entry {
    service_id: Option<u64>,
    url: Option<String>,
    name: Option<String>,
}

#[derive(Deserialize)]
#[serde(field_identifier, rename_all = "snake_case")]
enum Field {
    ServiceId,
    Url,
    Name,
    #[serde(other)]
    Other,
}

impl<'de> Deserialize<'de> for Entry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Fields;

        impl<'de> Visitor<'de> for Fields {
            type Value = Entry;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a subscription")
            }

            /// A field given twice keeps the last, as serde_json's own maps do.
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entry, A::Error> {
                let mut entry = Entry::default();
                while let Some(field) = map.next_key::<Field>()? {
                    match field {
                        Field::ServiceId => entry.service_id = map.next_value::<Scalar>()?.number(),
                        Field::Url => entry.url = map.next_value::<Scalar>()?.text(),
                        Field::Name => entry.name = map.next_value::<Scalar>()?.text(),
                        Field::Other => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(entry)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Entry, A::Error> {
                IgnoredAny.visit_seq(seq)?;
                Ok(Entry::default())
            }

            fn visit_str<E: de::Error>(self, _: &str) -> Result<Entry, E> {
                Ok(Entry::default())
            }

            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Entry, E> {
                Ok(Entry::default())
            }

            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Entry, E> {
                Ok(Entry::default())
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Entry, E> {
                Ok(Entry::default())
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Entry, E> {
                Ok(Entry::default())
            }

            fn visit_unit<E: de::Error>(self) -> Result<Entry, E> {
                Ok(Entry::default())
            }
        }

        deserializer.deserialize_any(Fields)
    }
}

/// A field's value: a whole number that isn't negative, text, or something
/// else, which is stepped over and not kept.
enum Scalar {
    Number(u64),
    Text(String),
    Other,
}

impl Scalar {
    fn number(self) -> Option<u64> {
        match self {
            Self::Number(number) => Some(number),
            _ => None,
        }
    }

    fn text(self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Any;

        impl<'de> Visitor<'de> for Any {
            type Value = Scalar;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a value")
            }

            fn visit_u64<E: de::Error>(self, number: u64) -> Result<Scalar, E> {
                Ok(Scalar::Number(number))
            }

            fn visit_i64<E: de::Error>(self, number: i64) -> Result<Scalar, E> {
                Ok(u64::try_from(number).map_or(Scalar::Other, Scalar::Number))
            }

            fn visit_str<E: de::Error>(self, text: &str) -> Result<Scalar, E> {
                Ok(Scalar::Text(text.to_owned()))
            }

            fn visit_string<E: de::Error>(self, text: String) -> Result<Scalar, E> {
                Ok(Scalar::Text(text))
            }

            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Scalar, E> {
                Ok(Scalar::Other)
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Scalar, E> {
                Ok(Scalar::Other)
            }

            fn visit_unit<E: de::Error>(self) -> Result<Scalar, E> {
                Ok(Scalar::Other)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<Scalar, A::Error> {
                IgnoredAny.visit_seq(seq)?;
                Ok(Scalar::Other)
            }

            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Scalar, A::Error> {
                IgnoredAny.visit_map(map)?;
                Ok(Scalar::Other)
            }
        }

        deserializer.deserialize_any(Any)
    }
}

/// A channel from one entry, or None when it isn't YouTube's or has no id.
/// The name may be missing or not text, as a title may be in Google's file:
/// the channel is fetched after, and that fills it in.
fn channel(entry: &Entry, now: i64) -> Option<Channel> {
    if entry.service_id? != YOUTUBE {
        return None;
    }
    let ChannelInput::Id(id) = ChannelInput::parse(entry.url.as_deref()?)? else {
        return None;
    };
    Some(Channel {
        title: entry.name.as_deref().map(clean_text).unwrap_or_default(),
        added_at: now,
        ..Channel::new(id)
    })
}

/// Reads NewPipe's file from its text. JSON of any other shape is refused as a
/// whole, while a bad entry is only counted.
pub fn parse(text: &str, now: i64) -> Result<Parsed, ImportError> {
    let export: Export = serde_json::from_str(text).map_err(|_| ImportError::NotNewPipe)?;
    let mut skipped = export.subscriptions.more;
    let channels = export
        .subscriptions
        .kept
        .iter()
        .filter_map(|entry| {
            let channel = channel(entry, now);
            skipped += usize::from(channel.is_none());
            channel
        })
        .collect();
    Ok(Parsed {
        rows: Rows::Channels(channels),
        skipped,
    })
}

#[derive(Serialize)]
struct ExportOut<'a> {
    subscriptions: Vec<EntryOut<'a>>,
    app_version: &'static str,
    app_version_int: u32,
}

#[derive(Serialize)]
struct EntryOut<'a> {
    service_id: u64,
    url: String,
    name: &'a str,
}

/// The channels as NewPipe's file: its keys and no others, since NewPipe
/// refuses a file with a key it doesn't know, on one line. The list comes
/// first, as 0.29.1 writes it; older releases put it last, and NewPipe reads
/// the keys in any order.
pub fn write_subscriptions(channels: &[Channel]) -> Vec<u8> {
    let export = ExportOut {
        subscriptions: channels
            .iter()
            .map(|channel| EntryOut {
                service_id: YOUTUBE,
                url: channel.id.url(),
                name: &channel.title,
            })
            .collect(),
        app_version: APP_VERSION,
        app_version_int: APP_VERSION_INT,
    };
    // Plain structs with names for keys, which can't fail.
    serde_json::to_vec(&export).unwrap_or_else(|error| {
        log::warn!("could not write the list: {error}");
        Vec::new()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = include_str!("fixtures/newpipe-subscriptions.json");
    const MIXED: &str = include_str!("fixtures/newpipe-mixed.json");
    const A: &str = "UCaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "UCbbbbbbbbbbbbbbbbbbbbbb";
    const NOW: i64 = 1_800_000_000;

    fn channels(text: &str) -> (Vec<(String, String)>, usize) {
        let parsed = parse(text, NOW).unwrap();
        let Rows::Channels(rows) = parsed.rows else {
            panic!("not channels");
        };
        assert!(rows.iter().all(|channel| channel.added_at == NOW));
        (
            rows.into_iter()
                .map(|channel| (channel.id.as_str().to_owned(), channel.title))
                .collect(),
            parsed.skipped,
        )
    }

    fn pair(id: &str, title: &str) -> (String, String) {
        (id.to_owned(), title.to_owned())
    }

    fn file(entries: &str) -> String {
        format!(r#"{{"app_version":"0.29.1","app_version_int":1015,"subscriptions":[{entries}]}}"#)
    }

    /// The fixture has the list first, as 0.29.1 writes it. Older releases
    /// wrote the version numbers first, and that reads the same.
    #[test]
    fn a_file_as_newpipe_writes_it_reads() {
        let expected = vec![
            pair("UCq3Zr1Lw8NfTb5Yx0Ck2HmA", "Kettle and Kiln"),
            pair("UC7pDs0Wn4JcXe2Ty9Rb6LuQ", "Orbital Notes – Space, Simply"),
            pair("UCm9Hf2Kq5VbZa8Ls1Nd4WxE", "Ведомости мастерской"),
            pair("UC0xYt6Gr3PlQw9Ec2Mv5KjS", "手作りの台所"),
        ];
        assert!(REAL.starts_with(r#"{"subscriptions":["#));
        assert_eq!(channels(REAL), (expected.clone(), 0));
        let list = &REAL[REAL.find('[').unwrap()..REAL.rfind(']').unwrap() + 1];
        let older =
            format!(r#"{{"app_version":"0.28.0","app_version_int":1002,"subscriptions":{list}}}"#);
        assert_eq!(channels(&older), (expected, 0));
    }

    /// Other services, handles and broken entries are counted and skipped,
    /// and never cost the rest.
    #[test]
    fn what_cant_be_used_is_counted_and_skipped() {
        let (rows, skipped) = channels(MIXED);
        assert_eq!(
            rows,
            [
                pair("UCq3Zr1Lw8NfTb5Yx0Ck2HmA", "Kettle and Kiln"),
                pair("UC7pDs0Wn4JcXe2Ty9Rb6LuQ", "Plain http"),
                pair("UCm9Hf2Kq5VbZa8Ls1Nd4WxE", "No www, a slash and a query"),
                pair("UC0xYt6Gr3PlQw9Ec2Mv5KjS", "Mobile"),
                pair("UCe4Nw7Bt1KsRy3Pz6Fh0GcD", ""),
                pair("UCj8Ux2Lm5QdWv0Ha3Tn7YbK", ""),
                pair("UCr1Gk4Sy7XcBn2Me5Pq9VwZ", "abc"),
            ]
        );
        assert_eq!(skipped, 17);
    }

    #[test]
    fn json_of_another_shape_is_refused() {
        for text in [
            "{}",
            "[]",
            r#"[{"service_id":0,"url":"https://www.youtube.com/channel/UCaaaaaaaaaaaaaaaaaaaaaa","name":"x"}]"#,
            r#"{"subscriptions":null}"#,
            r#"{"subscriptions":{"service_id":0}}"#,
            r#"{"subscriptions":"none"}"#,
            r#"{"app_version":"0.29.1","app_version_int":1015}"#,
            r#"{"subscriptions":[]"#,
            r#"{"subscriptions":[{"service_id":0,}]}"#,
            "{\"subscriptions\":[]} trailing",
            "{not json at all",
        ] {
            assert_eq!(parse(text, NOW), Err(ImportError::NotNewPipe), "{text}");
        }
    }

    #[test]
    fn an_empty_list_is_an_empty_import() {
        assert_eq!(channels(&file("")), (vec![], 0));
        // NewPipe itself doesn't need its version numbers.
        assert_eq!(channels(r#"{"subscriptions":[]}"#), (vec![], 0));
    }

    /// The url is read as any channel address typed into Patina is, so a
    /// music.youtube.com address, one with no scheme and a bare id are taken
    /// too. Each is only read for its id.
    #[test]
    fn every_form_of_a_channel_address_with_an_id_is_taken() {
        let urls = [
            format!("https://music.youtube.com/channel/{A}"),
            format!("youtube.com/channel/{A}"),
            format!("www.youtube.com/channel/{A}/"),
            A.to_owned(),
        ];
        let entries: Vec<String> = urls
            .iter()
            .map(|url| format!(r#"{{"service_id":0,"url":"{url}","name":"x"}}"#))
            .collect();
        let (rows, skipped) = channels(&file(&entries.join(",")));
        assert_eq!(rows, vec![pair(A, "x"); urls.len()]);
        assert_eq!(skipped, 0);
    }

    #[test]
    fn titles_are_cleaned_and_addresses_built_again() {
        let text = file(&format!(
            r#"{{"service_id":0,"url":"https://youtube.com/channel/{A}/videos?x=1#y","name":"a\u202eb\u001bc"}}"#
        ));
        let Rows::Channels(rows) = parse(&text, NOW).unwrap().rows else {
            panic!();
        };
        assert_eq!(rows[0].title, "abc");
        assert_eq!(
            rows[0].id.url(),
            format!("https://www.youtube.com/channel/{A}")
        );
    }

    #[test]
    fn past_the_limit_entries_are_counted_as_skipped() {
        let entry = format!(
            r#"{{"service_id":0,"url":"https://www.youtube.com/channel/{A}","name":"x"}},"#
        );
        let mut entries = entry.repeat(ROW_LIMIT + 5);
        entries.pop();
        let (rows, skipped) = channels(&file(&entries));
        assert_eq!(rows.len(), ROW_LIMIT);
        assert_eq!(skipped, 5);
    }

    /// Nesting of any depth is stepped over without going deeper into the
    /// stack: in an entry, or in place of one, that entry is skipped, and an
    /// object where the list should be is refused like any other.
    #[test]
    fn deep_nesting_is_skipped_or_refused_not_a_crash() {
        let deep = format!("{}0{}", "[".repeat(100_000), "]".repeat(100_000));
        let deep_object = format!("{}0{}", r#"{"a":"#.repeat(100_000), "}".repeat(100_000));
        for nested in [&deep, &deep_object] {
            let in_a_field = file(&format!(
                r#"{{"service_id":0,"url":{nested},"name":"x"}},{{"service_id":0,"url":"https://www.youtube.com/channel/{A}","extra":{nested}}}"#
            ));
            assert_eq!(channels(&in_a_field), (vec![pair(A, "")], 1));
            assert_eq!(channels(&file(nested)), (vec![], 1));
        }
        assert_eq!(
            parse(&format!(r#"{{"subscriptions":{deep_object}}}"#), NOW),
            Err(ImportError::NotNewPipe)
        );
    }

    /// What an entry holds besides its three fields is stepped over, not
    /// kept: one field of a wrong file could otherwise take most of the 32 MB
    /// a file may have, and cost a hundred times that once read.
    #[test]
    fn a_huge_field_in_an_entry_is_stepped_over() {
        let extra = format!("[{}{{}}]", r#"{"":0},"#.repeat(200_000));
        let text = file(&format!(
            r#"{{"service_id":0,"url":"https://www.youtube.com/channel/{A}","name":"x","extra":{extra}}},
            {{"service_id":0,"extra":{extra},"url":"https://www.youtube.com/channel/{B}","name":{extra}}}"#
        ));
        assert_eq!(channels(&text), (vec![pair(A, "x"), pair(B, "")], 0));
    }

    /// A field given twice keeps the last, wrong types included.
    #[test]
    fn a_repeated_field_keeps_the_last() {
        let text = file(&format!(
            r#"{{"service_id":1,"service_id":0,"url":"https://www.youtube.com/channel/{A}","name":"x","name":"y"}},
            {{"service_id":0,"url":"https://www.youtube.com/channel/{B}","url":null,"name":"z"}},
            {{"service_id":0,"service_id":-1,"url":"https://www.youtube.com/channel/{B}","name":"z"}},
            {{"service_id":0.0,"url":"https://www.youtube.com/channel/{B}","name":"z"}}"#
        ));
        assert_eq!(channels(&text), (vec![pair(A, "y")], 3));
    }

    #[test]
    fn is_json_tells_the_two_kinds_apart() {
        assert!(is_json(REAL));
        assert!(is_json("  \n[1]"));
        assert!(!is_json("Channel Id,Channel Url,Channel Title\n"));
        assert!(!is_json(""));
    }

    /// What Patina writes has NewPipe's keys and nothing more, on one line,
    /// in the order 0.29.1 writes them.
    #[test]
    fn the_export_is_in_newpipes_own_form() {
        let mut first = Channel::new(super::super::ChannelId::parse(A).unwrap());
        first.title = "Say \"hi\", \\ ünïcode".to_owned();
        let second = Channel::new(super::super::ChannelId::parse(B).unwrap());
        let written = String::from_utf8(write_subscriptions(&[first, second])).unwrap();
        assert_eq!(
            written,
            format!(
                "{{\"subscriptions\":[\
                 {{\"service_id\":0,\"url\":\"https://www.youtube.com/channel/{A}\",\
                 \"name\":\"Say \\\"hi\\\", \\\\ ünïcode\"}},\
                 {{\"service_id\":0,\"url\":\"https://www.youtube.com/channel/{B}\",\
                 \"name\":\"\"}}],\"app_version\":\"0.29.1\",\"app_version_int\":1015}}"
            )
        );
        assert_eq!(
            write_subscriptions(&[]),
            br#"{"subscriptions":[],"app_version":"0.29.1","app_version_int":1015}"#
        );
    }

    #[test]
    fn an_export_reads_back_as_it_was() {
        let Rows::Channels(rows) = parse(REAL, NOW).unwrap().rows else {
            panic!();
        };
        let written = String::from_utf8(write_subscriptions(&rows)).unwrap();
        let Rows::Channels(again) = parse(&written, NOW).unwrap().rows else {
            panic!();
        };
        assert_eq!(again, rows);
        // The fixture is laid out as 0.29.1 writes it, and so is what Patina writes.
        assert_eq!(written, REAL.trim_end());
    }
}
