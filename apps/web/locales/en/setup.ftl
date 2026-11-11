## The first-run setup wizard (templates/setup/wizard.html). It runs before the site has a
## language, so it shows the base language.

setup-error-password-mismatch = The two administrator passwords do not match
setup-error-sqlite-path = Enter the path of the SQLite database file
setup-error-backend = Choose a database
# $detail is the underlying error.
setup-error-site-url = Forum address: { $detail }
setup-error-redis = Cannot connect to Redis: { $detail }
setup-error-directory = Cannot create the directory { $path }: { $detail }
setup-error-database = Cannot connect to the database: { $detail }
setup-error-tables = Cannot read the tables in the database: { $detail }
setup-error-foreign-tables = This database already has other tables. Please use an empty database (or a new SQLite file)
setup-error-migrate = Creating the tables failed: { $detail }
setup-error-installed = Gono Forum is already installed in this database. To keep using it, put the original configuration file (with its keys) back in place and start again; do not install again
setup-error-config-file = Cannot write the configuration file: { $detail }
setup-title = Install Gono Forum
setup-finished = Installation finished. The forum is starting and the login page opens in a few seconds.
# $url_html is the login address in code style.
setup-finished-manual = If nothing happens, open { $url_html } yourself and log in with the administrator account you just set up.
# $path_html is the path of the token file on the server.
setup-token-lead = This forum is not installed yet. Enter the setup token: the install script printed it in the terminal, and it is also saved on the server in { $path_html }.
setup-token-rejected = That token is wrong. Please copy it from { $path_html }.
setup-token = Setup token
setup-continue = Continue
# $path_html is the path of the private configuration file that setup writes.
setup-configure-lead = Choose a database and set up an administrator, and you are ready. Setup writes the private configuration file { $path_html } with three randomly generated keys; back it up together with the database.
setup-admin-lead = The configuration file is written, but the database has no administrator yet. The forum starts once you set up the administrator account.
setup-database = Database
setup-sqlite = SQLite (one server, one process, no database service to install)
setup-sqlite-path = Database file
setup-sqlite-hint = The file is created if it does not exist; the service account must be able to write to its directory.
setup-mysql = MySQL 5.7.44 or later
setup-url = Connection string
setup-mysql-placeholder = mysql://gonof:password@127.0.0.1:3306/gonof
setup-mysql-hint = Create the database first, with no tables, character set utf8mb4 and collation utf8mb4_bin; URL-encode special characters in the password.
setup-redis = Redis (optional)
setup-redis-enable = Use Redis
setup-redis-hint = Not needed for one server with one process; required when several processes share one database. Needs version 7.0 or later.
setup-redis-namespace = Key prefix
setup-redis-namespace-hint = Give each forum its own prefix when several share one Redis.
setup-site-url-heading = Forum address (optional)
setup-site-url = Forum address
# $example_html is an example address in code style.
setup-site-url-hint = The full address people use to open the forum; it can include a path (such as { $example_html }). Left empty, links follow the domain of each request; you can change it later in the admin area.
setup-admin = Administrator
setup-username = Username
setup-email = Email (optional)
setup-email-hint = Email sending is set up in the admin area after installation; the email entered here counts as verified.
setup-password = Password
setup-password-confirm = Password again
setup-password-hint = By default the site requires at least 12 characters; you can change this later in the admin area.
setup-install = Install
setup-create-admin = Create administrator
# The administrator stage of the setup wizard when an administrator already exists.
setup-error-admin-exists = The site already has an administrator
# Configuration checks shown by the setup wizard (and printed at startup).
setup-config-database-missing = database.url is missing: set it in the private configuration or in GONOF_DATABASE_URL (mysql:// for MySQL 5.7.44 or later, or sqlite:// for SQLite)
setup-config-database-url = database.url must be a valid database URL
setup-config-database-host = A mysql:// database.url must include the host
setup-config-database-name = database.url must name the database and cannot have a fragment
setup-config-sqlite-form = Write a SQLite database.url as sqlite:///absolute/path/file.db or sqlite://relative/path/file.db
setup-config-sqlite-memory = A SQLite database.url must point to a database file, not an in-memory database
setup-config-database-scheme = database.url only supports mysql:// (MySQL 5.7.44 or later) and sqlite:// (a SQLite database file)
setup-config-max-connections = database.max_connections must be greater than 0
setup-config-acquire-timeout = database.acquire_timeout_seconds must be greater than 0
setup-config-redis-missing = redis.url is missing: set it in the private configuration or in GONOF_REDIS_URL (required while Redis is enabled; version 7.0 or later)
setup-config-redis-url = redis.url must be a valid connection URL
setup-config-redis-scheme = redis.url must use redis:// or rediss://
setup-config-redis-namespace = redis.namespace may only contain letters, digits, colons, underscores and hyphens, 1–120 characters
setup-config-redis-timeout = redis.timeout_milliseconds must be greater than 0
setup-config-site-url = The site address must be a full URL
setup-config-site-scheme = The site address must use HTTP(S) and include a host name
setup-config-site-origin = The site address may only be an origin, without a path, query or account
setup-config-forum-url = The forum address must be a full URL
setup-config-forum-query = The forum address cannot have a query or fragment
setup-config-forum-path = Each segment of the forum address path may only use lowercase letters, digits and -, at most 64 characters
setup-config-mail-renamed = The [mail] section of the private configuration is now [common]: move encryption_key into [common] as it is; saved mail passwords need not be entered again
setup-config-sqlite-url = database.url is not a valid SQLite connection string
setup-config-sqlite-missing = The SQLite database file does not exist: { $file }. For a first installation, run gono-forum-cli migrate to create it
setup-config-sqlite-connect = Cannot connect to SQLite: { $file }
setup-config-sqlite-read-connect = Cannot open the read-only SQLite connection: { $file }
setup-config-sqlite-version = SQLite 3.46.0 or later is required; this is version { $version }
setup-config-mysql-connect = Cannot connect to MySQL
setup-config-mysql-version = MySQL 5.7.44 or later is required; this is version { $version }
setup-config-migrate = Database migration failed
setup-config-encryption-key = Set [common] encryption_key in the private configuration file next to the database settings (a random 64-character hexadecimal key; openssl rand -hex 32 makes one)
setup-time-zone-heading = Time zone
setup-time-zone = Site time zone
setup-time-zone-hint = Days and weeks (check-ins, daily limits, weekly contributors) are counted in this time zone. It starts as your browser's time zone; you can change it later in the admin area under Global settings › Site settings.
setup-error-time-zone = Choose a valid site time zone
setup-sample-heading = Sample content (optional)
setup-sample = Fill the forum with sample content
# $members, $forums, $threads and $replies are the numbers of sample members, forums, threads and replies.
setup-sample-hint = Adds { $members } members, { $forums } forums, { $threads } threads and { $replies } replies, all in English, so you can see how the forum looks before real members arrive. Nobody can sign in as the sample members. Leave it off for a forum that goes straight into use.
# The setup-sample checkbox was ticked for a database that already has content.
setup-error-sample-content = This database already has forums or posts, so the sample content cannot be added. Untick "Fill the forum with sample content" to continue.
setup-config-encryption-key-reused = [common] encryption_key must differ from the identity.cookie and identity.two_factor keys
