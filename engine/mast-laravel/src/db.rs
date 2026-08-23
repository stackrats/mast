//! Database credential doctoring — the domain half of Sail's worst trap.
//!
//! MySQL/MariaDB/Postgres images create the user, password and database
//! **only when their data volume is first initialized**; every later `.env`
//! edit silently changes nothing, and the app greets the developer with
//! "Access denied for user 'sail'@'%'" until they discover `sail down -v`
//! (destroying their data) or hand-run GRANTs. This module builds everything
//! pure: credential extraction from `.env`, probe argv tails, failure
//! classification, and the reconcile SQL an admin login can apply live. The
//! engine owns the effects (compose exec, streaming, consent).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DbKind {
    Mysql,
    Mariadb,
    Pgsql,
}

impl DbKind {
    pub fn parse(connection: &str) -> Option<Self> {
        match connection.trim() {
            "mysql" => Some(Self::Mysql),
            "mariadb" => Some(Self::Mariadb),
            "pgsql" => Some(Self::Pgsql),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mysql => "mysql",
            Self::Mariadb => "mariadb",
            Self::Pgsql => "pgsql",
        }
    }
}

/// The credentials `.env` declares for the app's database connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DbCreds {
    pub kind: DbKind,
    /// DB_HOST — a compose service name or alias when Sail-shaped.
    pub host: String,
    pub database: String,
    pub username: String,
    /// May legitimately be empty (MYSQL_ALLOW_EMPTY_PASSWORD setups).
    pub password: String,
}

/// Extract probe-able credentials from `.env` pairs. `None` when the
/// connection is not one of the three volume-initialized engines, when a
/// required field is missing, or when DB_HOST points at the host machine —
/// that misconfiguration belongs to the hostname validator, and there is no
/// service container to probe through.
pub fn db_creds(entries: &[(String, String)]) -> Option<DbCreds> {
    let get = |key: &str| {
        entries
            .iter()
            .rev() // last assignment wins, like dotenv itself
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.trim().to_string())
    };
    let kind = DbKind::parse(&get("DB_CONNECTION")?)?;
    let host = get("DB_HOST").filter(|h| !h.is_empty() && !h.contains('$'))?;
    if matches!(host.as_str(), "localhost" | "127.0.0.1" | "host.docker.internal") {
        return None;
    }
    let database = get("DB_DATABASE").filter(|v| !v.is_empty() && !v.contains('$'))?;
    let username = get("DB_USERNAME").filter(|v| !v.is_empty() && !v.contains('$'))?;
    let password = get("DB_PASSWORD").unwrap_or_default();
    if password.contains('$') {
        return None; // interpolated — we cannot know the real value
    }
    Some(DbCreds { kind, host, database, username, password })
}

/// How a credential probe failed, when it failed for a reason the
/// init-once-volume trap explains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailure {
    /// Wrong user or password (or missing grant) — the volume was
    /// initialized with different credentials.
    AccessDenied,
    /// The declared database does not exist in the volume.
    UnknownDatabase,
}

/// Classify a failed client invocation. `None` means "not a credential
/// problem" (service not running, network hiccup, …) — no finding.
pub fn classify_probe(kind: DbKind, stderr: &str) -> Option<ProbeFailure> {
    match kind {
        DbKind::Mysql | DbKind::Mariadb => {
            if stderr.contains("Access denied") {
                Some(ProbeFailure::AccessDenied)
            } else if stderr.contains("Unknown database") {
                Some(ProbeFailure::UnknownDatabase)
            } else {
                None
            }
        }
        DbKind::Pgsql => {
            if stderr.contains("password authentication failed")
                || stderr.contains("no password supplied")
                || stderr_role_missing(stderr)
            {
                Some(ProbeFailure::AccessDenied)
            } else if stderr.contains("database") && stderr.contains("does not exist") {
                Some(ProbeFailure::UnknownDatabase)
            } else {
                None
            }
        }
    }
}

fn stderr_role_missing(stderr: &str) -> bool {
    stderr.contains("role") && stderr.contains("does not exist")
}

/// The client binary present in the service's own image (mariadb images ship
/// `mariadb`; the `mysql` symlink is deprecated there and noisy).
pub fn client_binary(kind: DbKind) -> &'static str {
    match kind {
        DbKind::Mysql => "mysql",
        DbKind::Mariadb => "mariadb",
        DbKind::Pgsql => "psql",
    }
}

/// Command tail run inside the db service container to test the app's own
/// credentials, plus the env vars that carry the password (env, not argv, so
/// the secret never sits in the container's process list).
pub fn probe_tail(creds: &DbCreds) -> (Vec<String>, Vec<(String, String)>) {
    let client = client_binary(creds.kind);
    match creds.kind {
        DbKind::Mysql | DbKind::Mariadb => (
            [client, "-u", &creds.username, "-D", &creds.database, "-N", "-e", "SELECT 1"]
                .map(String::from)
                .to_vec(),
            vec![("MYSQL_PWD".into(), creds.password.clone())],
        ),
        // -w: fail rather than prompt (stdin is closed under exec -T).
        DbKind::Pgsql => (
            [client, "-w", "-U", &creds.username, "-d", &creds.database, "-tAc", "SELECT 1"]
                .map(String::from)
                .to_vec(),
            vec![("PGPASSWORD".into(), creds.password.clone())],
        ),
    }
}

/// One way an administrative login might still work on the initialized
/// volume, tried in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdminLogin {
    pub user: String,
    /// `None` = no password attempted (empty for mysql, socket trust for pg).
    pub password: Option<String>,
    /// Human answer to "why might this work?" for op output.
    pub rationale: &'static str,
}

/// Admin logins worth trying, most likely first.
///
/// mysql/mariadb: root's password is whatever DB_PASSWORD was **at volume
/// init**. If only user/database changed, that is still the current value;
/// empty covers MYSQL_ALLOW_EMPTY_PASSWORD setups. Postgres is friendlier:
/// the official image's entrypoint runs initdb with `--auth-local=trust`, so
/// a socket connection from inside the container needs no password at all —
/// only the role name (the init-time POSTGRES_USER, or `postgres`).
pub fn admin_logins(creds: &DbCreds) -> Vec<AdminLogin> {
    match creds.kind {
        DbKind::Mysql | DbKind::Mariadb => vec![
            AdminLogin {
                user: "root".into(),
                password: Some(creds.password.clone()),
                rationale: "root with the current DB_PASSWORD (set when the volume was created)",
            },
            AdminLogin {
                user: "root".into(),
                password: None,
                rationale: "root with an empty password",
            },
        ],
        DbKind::Pgsql => vec![
            AdminLogin {
                user: creds.username.clone(),
                password: None,
                rationale: "the current DB_USERNAME over the trusted local socket",
            },
            AdminLogin {
                user: "postgres".into(),
                password: None,
                rationale: "the postgres superuser over the trusted local socket",
            },
        ],
    }
}

/// Command tail for one admin login attempt (`SELECT 1` as that admin).
pub fn admin_probe_tail(kind: DbKind, login: &AdminLogin) -> (Vec<String>, Vec<(String, String)>) {
    let client = client_binary(kind);
    match kind {
        DbKind::Mysql | DbKind::Mariadb => (
            [client, "-u", &login.user, "-N", "-e", "SELECT 1"].map(String::from).to_vec(),
            login.password.iter().map(|p| ("MYSQL_PWD".to_string(), p.clone())).collect(),
        ),
        // `postgres` always exists after initdb; the role's own default
        // database may not.
        DbKind::Pgsql => (
            [client, "-w", "-U", &login.user, "-d", "postgres", "-tAc", "SELECT 1"]
                .map(String::from)
                .to_vec(),
            Vec::new(),
        ),
    }
}

/// Command tail applying `sql` as `login` (statements batch for mysql-family,
/// a single `-c` for postgres against the maintenance db).
pub fn admin_sql_tail(
    kind: DbKind,
    login: &AdminLogin,
    sql: &str,
) -> (Vec<String>, Vec<(String, String)>) {
    let client = client_binary(kind);
    match kind {
        DbKind::Mysql | DbKind::Mariadb => (
            [client, "-u", &login.user, "-e", sql].map(String::from).to_vec(),
            login.password.iter().map(|p| ("MYSQL_PWD".to_string(), p.clone())).collect(),
        ),
        DbKind::Pgsql => (
            [client, "-w", "-U", &login.user, "-d", "postgres", "-c", sql]
                .map(String::from)
                .to_vec(),
            Vec::new(),
        ),
    }
}

/// Command tail running `query` and printing its bare result (for the
/// pg database-existence gate).
pub fn admin_query_tail(
    kind: DbKind,
    login: &AdminLogin,
    query: &str,
) -> (Vec<String>, Vec<(String, String)>) {
    let client = client_binary(kind);
    match kind {
        DbKind::Mysql | DbKind::Mariadb => (
            [client, "-u", &login.user, "-N", "-e", query].map(String::from).to_vec(),
            login.password.iter().map(|p| ("MYSQL_PWD".to_string(), p.clone())).collect(),
        ),
        DbKind::Pgsql => (
            [client, "-w", "-U", &login.user, "-d", "postgres", "-tAc", query]
                .map(String::from)
                .to_vec(),
            Vec::new(),
        ),
    }
}

/// Identifier discipline: everything we splice into SQL as a name must be
/// boring. `.env` values outside this alphabet get a refusal, not an escape
/// adventure.
pub fn valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn mysql_literal(value: &str) -> String {
    // '' doubling works in every sql_mode; backslash doubling covers the
    // default mode. NO_BACKSLASH_ESCAPES + backslash-in-password is the one
    // combination this misencodes — vanishingly rare in a dev .env.
    format!("'{}'", value.replace('\\', "\\\\").replace('\'', "''"))
}

fn pg_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The statements a working mysql/mariadb root login applies to make the
/// volume agree with `.env`. Mirrors what the image + Sail's init scripts
/// would have done on a fresh volume: database, user, grant — plus the
/// `testing%` grant Sail ships for parallel test databases (laravel/sail
/// #112/#843 fall out of the same trap).
pub fn mysql_reconcile_sql(creds: &DbCreds) -> Result<String, String> {
    if !valid_identifier(&creds.database) {
        return Err(format!("DB_DATABASE \"{}\" is not a plain identifier", creds.database));
    }
    if !valid_identifier(&creds.username) {
        return Err(format!("DB_USERNAME \"{}\" is not a plain identifier", creds.username));
    }
    let db = &creds.database;
    let user = format!("'{}'@'%'", creds.username);
    let pwd = mysql_literal(&creds.password);
    Ok(format!(
        "CREATE DATABASE IF NOT EXISTS `{db}`;\n\
         CREATE USER IF NOT EXISTS {user} IDENTIFIED BY {pwd};\n\
         ALTER USER {user} IDENTIFIED BY {pwd};\n\
         GRANT ALL PRIVILEGES ON `{db}`.* TO {user};\n\
         CREATE DATABASE IF NOT EXISTS `testing`;\n\
         GRANT ALL PRIVILEGES ON `testing%`.* TO {user};\n\
         FLUSH PRIVILEGES;"
    ))
}

/// Postgres role reconcile (run against the `postgres` maintenance db).
/// CREATE DATABASE cannot live in a DO block, so database creation is a
/// separate statement gated on [`pg_database_missing_query`].
pub fn pg_role_sql(creds: &DbCreds) -> Result<String, String> {
    if !valid_identifier(&creds.username) {
        return Err(format!("DB_USERNAME \"{}\" is not a plain identifier", creds.username));
    }
    let user = &creds.username;
    let pwd = pg_literal(&creds.password);
    // Superuser matches what the postgres image grants POSTGRES_USER at init.
    Ok(format!(
        "DO $$ BEGIN \
         IF EXISTS (SELECT FROM pg_roles WHERE rolname = {rolname}) THEN \
         ALTER ROLE \"{user}\" WITH LOGIN PASSWORD {pwd}; \
         ELSE \
         CREATE ROLE \"{user}\" WITH LOGIN SUPERUSER PASSWORD {pwd}; \
         END IF; \
         END $$;",
        rolname = pg_literal(user),
    ))
}

/// Prints `missing` when the database does not exist (empty output = exists).
pub fn pg_database_missing_query(database: &str) -> String {
    format!(
        "SELECT 'missing' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = {})",
        pg_literal(database)
    )
}

pub fn pg_create_database_sql(creds: &DbCreds) -> Result<String, String> {
    if !valid_identifier(&creds.database) {
        return Err(format!("DB_DATABASE \"{}\" is not a plain identifier", creds.database));
    }
    if !valid_identifier(&creds.username) {
        return Err(format!("DB_USERNAME \"{}\" is not a plain identifier", creds.username));
    }
    Ok(format!("CREATE DATABASE \"{}\" OWNER \"{}\"", creds.database, creds.username))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(list: &[(&str, &str)]) -> Vec<(String, String)> {
        list.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    fn sail_env() -> Vec<(String, String)> {
        pairs(&[
            ("DB_CONNECTION", "mysql"),
            ("DB_HOST", "mysql"),
            ("DB_PORT", "3306"),
            ("DB_DATABASE", "laravel"),
            ("DB_USERNAME", "sail"),
            ("DB_PASSWORD", "password"),
        ])
    }

    #[test]
    fn creds_extracted_from_sail_shaped_env() {
        let creds = db_creds(&sail_env()).unwrap();
        assert_eq!(creds.kind, DbKind::Mysql);
        assert_eq!(creds.host, "mysql");
        assert_eq!(creds.database, "laravel");
        assert_eq!(creds.username, "sail");
        assert_eq!(creds.password, "password");
    }

    #[test]
    fn creds_skip_non_service_hosts_and_foreign_engines() {
        let mut env = sail_env();
        env[1].1 = "127.0.0.1".into();
        assert!(db_creds(&env).is_none(), "host-machine DB is the hostname check's problem");

        let mut env = sail_env();
        env[0].1 = "sqlite".into();
        assert!(db_creds(&env).is_none());

        let mut env = sail_env();
        env[5].1 = "${SECRET_FROM_VAULT}".into();
        assert!(db_creds(&env).is_none(), "interpolated password is unknowable");

        let mut env = sail_env();
        env[3].1 = String::new();
        assert!(db_creds(&env).is_none(), "empty database is not probeable");
    }

    #[test]
    fn probe_failures_classified_per_engine() {
        let mysql = |s: &str| classify_probe(DbKind::Mysql, s);
        assert_eq!(
            mysql("ERROR 1045 (28000): Access denied for user 'sail'@'%' (using password: YES)"),
            Some(ProbeFailure::AccessDenied)
        );
        assert_eq!(
            mysql("ERROR 1044 (42000): Access denied for user 'sail'@'%' to database 'newdb'"),
            Some(ProbeFailure::AccessDenied)
        );
        assert_eq!(
            mysql("ERROR 1049 (42000): Unknown database 'newdb'"),
            Some(ProbeFailure::UnknownDatabase)
        );
        assert_eq!(mysql("ERROR 2002 (HY000): Can't connect to local MySQL server"), None);
        assert_eq!(mysql("service \"mysql\" is not running"), None);

        let pg = |s: &str| classify_probe(DbKind::Pgsql, s);
        assert_eq!(
            pg("psql: error: connection to server ... FATAL:  password authentication failed for user \"sail\""),
            Some(ProbeFailure::AccessDenied)
        );
        assert_eq!(
            pg("FATAL:  role \"newuser\" does not exist"),
            Some(ProbeFailure::AccessDenied)
        );
        assert_eq!(
            pg("FATAL:  database \"newdb\" does not exist"),
            Some(ProbeFailure::UnknownDatabase)
        );
        assert_eq!(pg("psql: error: could not connect to server"), None);
    }

    #[test]
    fn probe_tails_keep_the_password_in_env_not_argv() {
        let creds = db_creds(&sail_env()).unwrap();
        let (tail, env) = probe_tail(&creds);
        assert_eq!(tail, ["mysql", "-u", "sail", "-D", "laravel", "-N", "-e", "SELECT 1"]);
        assert_eq!(env, vec![("MYSQL_PWD".to_string(), "password".to_string())]);
        assert!(!tail.iter().any(|a| a.contains("password")));

        let mut pg_env = sail_env();
        pg_env[0].1 = "pgsql".into();
        pg_env[1].1 = "pgsql".into();
        let creds = db_creds(&pg_env).unwrap();
        let (tail, env) = probe_tail(&creds);
        assert_eq!(tail[0], "psql");
        assert!(tail.contains(&"-w".to_string()), "psql must never prompt: {tail:?}");
        assert_eq!(env[0].0, "PGPASSWORD");
    }

    #[test]
    fn admin_logins_match_each_engines_reality() {
        let creds = db_creds(&sail_env()).unwrap();
        let logins = admin_logins(&creds);
        assert_eq!(logins.len(), 2);
        assert_eq!(logins[0].user, "root");
        assert_eq!(logins[0].password.as_deref(), Some("password"));
        assert_eq!(logins[1].password, None);
        let (tail, env) = admin_probe_tail(creds.kind, &logins[1]);
        assert_eq!(tail, ["mysql", "-u", "root", "-N", "-e", "SELECT 1"]);
        assert!(env.is_empty());

        let mut pg_env = sail_env();
        pg_env[0].1 = "pgsql".into();
        let creds = db_creds(&pg_env).unwrap();
        let logins = admin_logins(&creds);
        assert_eq!(logins[0].user, "sail");
        assert_eq!(logins[1].user, "postgres");
        let (tail, env) = admin_probe_tail(creds.kind, &logins[1]);
        // Socket trust: no password env, maintenance db, never prompt.
        assert!(env.is_empty());
        assert!(tail.contains(&"postgres".to_string()));
        assert!(tail.contains(&"-w".to_string()));
    }

    #[test]
    fn mysql_reconcile_mirrors_image_init_plus_sail_testing_grant() {
        let creds = db_creds(&sail_env()).unwrap();
        let sql = mysql_reconcile_sql(&creds).unwrap();
        assert!(sql.contains("CREATE DATABASE IF NOT EXISTS `laravel`;"));
        assert!(sql.contains("CREATE USER IF NOT EXISTS 'sail'@'%' IDENTIFIED BY 'password';"));
        assert!(sql.contains("ALTER USER 'sail'@'%' IDENTIFIED BY 'password';"));
        assert!(sql.contains("GRANT ALL PRIVILEGES ON `laravel`.* TO 'sail'@'%';"));
        // Sail's create-testing-database.sh shape — fixes parallel testing too.
        assert!(sql.contains("GRANT ALL PRIVILEGES ON `testing%`.* TO 'sail'@'%';"));
        assert!(sql.ends_with("FLUSH PRIVILEGES;"));
    }

    #[test]
    fn sql_builders_escape_or_refuse() {
        let mut creds = db_creds(&sail_env()).unwrap();
        creds.password = "it's a \\ trap".into();
        let sql = mysql_reconcile_sql(&creds).unwrap();
        assert!(sql.contains(r"'it''s a \\ trap'"), "{sql}");

        creds.database = "bad`name".into();
        assert!(mysql_reconcile_sql(&creds).unwrap_err().contains("DB_DATABASE"));

        let mut pg = db_creds(&sail_env()).unwrap();
        pg.kind = DbKind::Pgsql;
        pg.password = "o'clock".into();
        let role = pg_role_sql(&pg).unwrap();
        assert!(role.contains("'o''clock'"), "{role}");
        assert!(role.contains("CREATE ROLE \"sail\" WITH LOGIN SUPERUSER"));

        pg.username = "u;er".into();
        assert!(pg_role_sql(&pg).unwrap_err().contains("DB_USERNAME"));
        assert!(pg_create_database_sql(&pg).unwrap_err().contains("DB_USERNAME"));

        assert_eq!(
            pg_database_missing_query("new'db"),
            "SELECT 'missing' WHERE NOT EXISTS (SELECT FROM pg_database WHERE datname = 'new''db')"
        );
    }
}
