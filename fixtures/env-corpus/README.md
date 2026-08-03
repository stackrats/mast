# env-corpus

Docker dotenv syntax matrix (plan §6) for the lossless `.env` model in
`mast-laravel`. Byte-level traps are intentional (CRLF file, missing trailing
newline in weird.env, exact spacing) — formatters must never touch this
directory; add new traps as new files.
