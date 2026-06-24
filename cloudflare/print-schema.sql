PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS print_documents (
  id TEXT PRIMARY KEY,
  source_type TEXT NOT NULL DEFAULT 'web',
  source_name TEXT NOT NULL,
  display_name TEXT,
  mime_type TEXT NOT NULL,
  declared_size INTEGER NOT NULL,
  actual_size INTEGER,
  sha256 TEXT,
  r2_key TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL CHECK (
    status IN ('uploading', 'pending', 'downloading', 'converting', 'ready', 'failed', 'expired')
  ),
  error TEXT,
  page_count INTEGER,
  prepared_device_id TEXT,
  claim_device_id TEXT,
  claim_expires_at INTEGER,
  created_at INTEGER NOT NULL,
  uploaded_at INTEGER,
  prepared_at INTEGER,
  expires_at INTEGER NOT NULL,
  source_deleted_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_print_documents_pending
  ON print_documents(status, claim_expires_at, created_at);

CREATE INDEX IF NOT EXISTS idx_print_documents_expires
  ON print_documents(expires_at, status);

CREATE TABLE IF NOT EXISTS print_handoffs (
  token_hash TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES print_documents(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  consumed_at INTEGER
);

CREATE INDEX IF NOT EXISTS idx_print_handoffs_expires
  ON print_handoffs(expires_at, consumed_at);

CREATE TABLE IF NOT EXISTS print_jobs (
  id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL UNIQUE REFERENCES print_documents(id),
  target_device_id TEXT NOT NULL,
  user_name TEXT NOT NULL,
  file_name TEXT NOT NULL,
  page_count INTEGER NOT NULL,
  copy_count INTEGER NOT NULL,
  color_mode TEXT NOT NULL CHECK (color_mode IN ('bw', 'color')),
  price_per_page REAL NOT NULL,
  total_price REAL NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('queued', 'printing', 'done', 'failed')),
  detail TEXT,
  pages_printed INTEGER NOT NULL DEFAULT 0,
  total_pages INTEGER NOT NULL,
  claim_device_id TEXT,
  claim_expires_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_print_jobs_pending
  ON print_jobs(target_device_id, status, claim_expires_at, created_at);

CREATE TABLE IF NOT EXISTS print_events (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  kind TEXT NOT NULL,
  entity_id TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_print_events_created
  ON print_events(created_at);
