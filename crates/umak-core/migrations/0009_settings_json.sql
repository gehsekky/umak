-- Every setting is stored as JSON. The player kept some as plain text before.
UPDATE settings SET value = json_quote(value) WHERE NOT json_valid(value);
