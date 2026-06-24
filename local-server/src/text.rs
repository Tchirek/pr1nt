use chrono::Utc;

pub(crate) fn format_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;

    if (bytes as f64) >= MB {
        return format!("{:.1} MB", bytes as f64 / MB);
    }
    if (bytes as f64) >= KB {
        return format!("{:.1} KB", bytes as f64 / KB);
    }
    format!("{bytes} B")
}

pub(crate) fn now_iso() -> String {
    Utc::now().to_rfc3339()
}

pub(crate) fn queue_waiting_text() -> String {
    "\u{6392}\u{961f}\u{4e2d}".to_owned()
}

pub(crate) fn queue_position_text(position: usize) -> String {
    format!("\u{6392}\u{961f}\u{4e2d} (\u{7b2c}{position}\u{4f4d})")
}

pub(crate) fn file_transfer_text() -> String {
    "\u{6587}\u{4ef6}\u{4f20}\u{8f93}\u{4e2d}".to_owned()
}

pub(crate) fn printing_text(printer_name: &str) -> String {
    format!("\u{6253}\u{5370}\u{4e2d}\u{ff1a}{printer_name}")
}

pub(crate) fn done_text() -> String {
    "\u{5b8c}\u{6210} \u{2713}".to_owned()
}

pub(crate) fn admin_retry_text() -> String {
    "\u{7ba1}\u{7406}\u{5458}\u{5df2}\u{91cd}\u{8bd5}\u{4efb}\u{52a1}\u{3002}".to_owned()
}
