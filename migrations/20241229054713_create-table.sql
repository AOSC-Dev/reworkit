-- Add migration script here
CREATE TABLE IF NOT EXISTS build_result (
name TEXT NOT NULL,
arch TEXT NOT NULL,
success BOOLEAN NOT NULL,
log TEXT NOT NULL,
UNIQUE (name, arch)
);
