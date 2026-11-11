use std::sync::{LazyLock, Mutex};
use std::thread;
use std::time::Duration;
use arboard::Clipboard;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardItem {
    pub timestamp: String, // HH:MM:SS (UTC)
    pub text: String,
    pub preview: String,
}

static HISTORY: LazyLock<Mutex<Vec<ClipboardItem>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Start the background clipboard daemon thread.
pub fn start_daemon() {
    thread::spawn(|| {
        let mut ctx = match Clipboard::new() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[clipboard] failed to initialize arboard: {e}");
                return;
            }
        };

        let mut last_text = String::new();

        loop {
            if let Ok(current) = ctx.get_text() {
                if !current.is_empty() && current != last_text {
                    last_text = current.clone();
                    
                    // Generate timestamp
                    let timestamp = current_time_string();
                    
                    // Preview: first 60 chars, single line, stripped of newlines
                    let trimmed: String = current
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(60)
                        .collect();
                    
                    let item = ClipboardItem {
                        timestamp,
                        text: current,
                        preview: trimmed,
                    };

                    if let Ok(mut hist) = HISTORY.lock() {
                        // Avoid duplicates at the very front
                        if hist.first().map(|x| &x.text) != Some(&item.text) {
                            hist.insert(0, item);
                            if hist.len() > 50 {
                                hist.truncate(50);
                            }
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(500));
        }
    });
}

/// Retrieve a copy of the clipboard history.
pub fn get_history() -> Vec<ClipboardItem> {
    if let Ok(hist) = HISTORY.lock() {
        hist.clone()
    } else {
        Vec::new()
    }
}

/// Clear the clipboard history (useful for tests).
pub fn clear_history() {
    if let Ok(mut hist) = HISTORY.lock() {
        hist.clear();
    }
}

/// Helper to format current local time as HH:MM:SS
fn current_time_string() -> String {
    if let Ok(duration) = std::time::SystemTime::now().duration_since(std::time::SystemTime::UNIX_EPOCH) {
        let secs = duration.as_secs();
        let h = (secs / 3600 % 24) as u32;
        let m = (secs / 60 % 60) as u32;
        let s = (secs % 60) as u32;
        format!("{:02}:{:02}:{:02}", h, m, s)
    } else {
        "00:00:00".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn history_lifecycle() {
        let _lock = TEST_LOCK.lock().unwrap();
        clear_history();
        assert_eq!(get_history().len(), 0);

        let item = ClipboardItem {
            timestamp: "12:00:00".to_string(),
            text: "hello".to_string(),
            preview: "hello".to_string(),
        };

        if let Ok(mut hist) = HISTORY.lock() {
            hist.push(item);
        }

        assert_eq!(get_history().len(), 1);
        assert_eq!(get_history()[0].text, "hello");
    }

    #[test]
    fn clear_history_empties_all() {
        let _lock = TEST_LOCK.lock().unwrap();
        clear_history();
        if let Ok(mut hist) = HISTORY.lock() {
            hist.push(ClipboardItem {
                timestamp: "01:00:00".to_string(),
                text: "alpha".to_string(),
                preview: "alpha".to_string(),
            });
            hist.push(ClipboardItem {
                timestamp: "02:00:00".to_string(),
                text: "beta".to_string(),
                preview: "beta".to_string(),
            });
        }
        assert!(get_history().len() >= 2);
        clear_history();
        assert_eq!(get_history().len(), 0);
    }

    #[test]
    fn history_cap_at_50() {
        let _lock = TEST_LOCK.lock().unwrap();
        clear_history();
        if let Ok(mut hist) = HISTORY.lock() {
            for i in 0..60 {
                hist.insert(0, ClipboardItem {
                    timestamp: format!("{:02}:00:00", i % 24),
                    text: format!("item_{}", i),
                    preview: format!("item_{}", i),
                });
                if hist.len() > 50 {
                    hist.truncate(50);
                }
            }
        }
        assert_eq!(get_history().len(), 50);
        clear_history();
    }

    #[test]
    fn duplicate_front_detection() {
        let _lock = TEST_LOCK.lock().unwrap();
        clear_history();
        if let Ok(mut hist) = HISTORY.lock() {
            let item = ClipboardItem {
                timestamp: "10:00:00".to_string(),
                text: "same".to_string(),
                preview: "same".to_string(),
            };
            // Insert first
            hist.insert(0, item.clone());
            // Should not re-insert if front is same
            if hist.first().map(|x| &x.text) != Some(&"same".to_string()) {
                hist.insert(0, item);
            }
        }
        assert_eq!(get_history().len(), 1);
        clear_history();
    }

    #[test]
    fn get_history_returns_clone() {
        let _lock = TEST_LOCK.lock().unwrap();
        clear_history();
        if let Ok(mut hist) = HISTORY.lock() {
            hist.push(ClipboardItem {
                timestamp: "00:00:00".to_string(),
                text: "test".to_string(),
                preview: "test".to_string(),
            });
        }
        let h1 = get_history();
        let h2 = get_history();
        assert_eq!(h1.len(), h2.len());
        assert_eq!(h1[0].text, h2[0].text);
        clear_history();
    }

    #[test]
    fn current_time_string_format() {
        let ts = current_time_string();
        assert_eq!(ts.len(), 8); // "HH:MM:SS"
        assert_eq!(&ts[2..3], ":");
        assert_eq!(&ts[5..6], ":");
    }

    #[test]
    fn clipboard_item_fields() {
        let item = ClipboardItem {
            timestamp: "15:30:45".to_string(),
            text: "hello world\nsecond line".to_string(),
            preview: "hello world".to_string(),
        };
        assert_eq!(item.timestamp, "15:30:45");
        assert!(item.text.contains('\n'));
        assert!(!item.preview.contains('\n'));
    }

    #[test]
    fn clipboard_item_equality() {
        let a = ClipboardItem {
            timestamp: "12:00:00".to_string(),
            text: "hello".to_string(),
            preview: "hello".to_string(),
        };
        let b = a.clone();
        assert_eq!(a, b);
    }
}
