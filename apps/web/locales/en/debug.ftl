## The debug panel at the bottom of administrators' pages (Tools & maintenance › Maintenance › Debug panel)

# The panel heading next to the product name.
debug-title = Debug
debug-summary-sql = SQL { $n ->
    [one] { $n } record
   *[other] { $n } records
}
debug-summary-redis = Redis { $n ->
    [one] { $n } round trip
   *[other] { $n } round trips
}
debug-toggle = Expand / collapse
debug-metric-duration = Request time
debug-metric-duration-note = Server-side processing of this request
debug-metric-sql = SQL records
debug-records = { $n ->
    [one] { $n } record
   *[other] { $n } records
}
debug-metric-sql-note = Calls and driver batches added up; times may overlap
debug-metric-rss = Process memory · RSS
debug-metric-rss-change = Change during the request: { $change }
debug-metric-cpu = Process CPU time
debug-metric-cpu-split = User { $user } · system { $system }
debug-note-timing = SQL times include waiting for a connection and reading the results, and driver batch times may overlap the calls around them. RSS is the resident memory of the process and CPU is the change in the process's total CPU time during the request; both include concurrent requests and background tasks, not this page alone.
debug-note-index = Index diagnostics took another { $duration }, not counted in the request and SQL times above. EXPLAIN plans come from the current state of the diagnostic connection, may differ from the original transaction or cached plans, and are not execution statistics.
debug-blocks-heading = In-process cache
debug-blocks-note = { $hits ->
    [one] { $hits } block hit
   *[other] { $hits } blocks hit
}: this request read them without querying the database, so they do not appear in the SQL below. To see all SQL, set the in-process cache lifetime in “Global settings › Search & performance” to 0
debug-blocks-empty = This request did not read the in-process cache.
debug-block-source = In-process
debug-block-hit = From the in-process cache
debug-block-filled = Read from the database and cached
debug-block-uncached = Lifetime is 0: read from the database, not cached
debug-block-age = Loaded { $seconds ->
    [one] { $seconds } second ago
   *[other] { $seconds } seconds ago
}
debug-redis-counts = In the order sent · { $hits } hit / { $misses } missed · { $duration } in total
debug-redis-note = Only command names and keys are recorded: a SET writes a whole page of HTML or a list snapshot, and the value stays out of the panel.
debug-redis-empty = This request did not use Redis.
debug-redis-hit = Hit
debug-redis-miss = Miss
debug-sql-note = Application calls in start order, driver batches in log order · select the text to copy a full SQL statement
debug-sql-empty = This request ran no SQL queries.
debug-driver-batch = SQLx driver batch
debug-driver-elapsed = Driver time { $elapsed }
debug-driver-no-result = No result reported
debug-driver-rows = Rows returned by the driver { $returned } · affected { $affected }
debug-query-ok = Succeeded
debug-query-failed = Failed
debug-query-cancelled = Cancelled
debug-rows = Rows returned { $returned } · affected { $affected }
debug-copy-label = Copy SQL #{ $n }
debug-copy = Copy SQL
debug-index-label = Indexes (EXPLAIN plan):{" "}
debug-driver-note = An internal SQLx setup batch; the driver log reports neither success nor failure, so judge by the call around it.
debug-expansion = Parameters cannot be expanded: { $reason }. The parameterized SQL is kept above; the bound parameters in full:
debug-database-error = Database error: { $error }
debug-cancelled-note = This SQL call was cancelled before it finished.
debug-unavailable = Unavailable
# Index diagnostics (EXPLAIN) of one query.
debug-explain-not-collected = No plan collected
debug-explain-skipped = Not analyzed: { $reason }
debug-explain-unavailable = Cannot analyze: { $reason }
debug-explain-budget-spent = The 750 ms diagnostic budget of this request is used up
debug-explain-query-limit = This request already analyzed 32 different queries
debug-explain-driver = A driver batch has no separate statement parameters to rebind
debug-explain-failed = The original query failed
debug-explain-cancelled = The original query was cancelled
debug-explain-parameters = The parameters of the original query cannot be matched reliably
debug-explain-sql-too-long = The SQL exceeds the 64 KiB diagnostic limit
debug-explain-parameters-too-long = The bound parameters exceed the 64 KiB diagnostic limit
debug-explain-multiple = Multiple statements or trailing comments are not analyzed
debug-explain-comment = The SQL has comments; skipped to stay clear of server-specific meaning
debug-explain-backslash = A SQL literal contains a backslash, so token boundaries cannot be judged reliably
debug-explain-quote = Unclosed SQL quote
debug-explain-lexical = The SQL uses an unsupported lexical form
debug-explain-variables = Statements with variables, dollar quotes or named parameters are not analyzed
debug-explain-select-only = Only a single SELECT is analyzed; WITH, writes, transactions and maintenance statements are not replayed
debug-explain-schema-function = Schema-qualified functions are not analyzed, to avoid calling user-defined functions
debug-explain-subquery-write = The SELECT contains a write, maintenance or unsupported subquery
debug-explain-subquery-read-only = The SELECT contains a statement that is not read-only
debug-explain-function = Contains a function or table function outside the read-only allowlist
debug-explain-quoted-function = Statements with quoted function names are not analyzed
debug-explain-remaining-spent = The remaining diagnostic budget of this request is used up
debug-explain-timeout = One EXPLAIN took more than 150 ms (including the connection wait)
debug-explain-other-database = This record was not run by the current database
debug-explain-connection = Cannot get a diagnostic connection: { $error }
debug-explain-mysql-only = Only MySQL supports EXPLAIN FORMAT=TRADITIONAL
debug-explain-output = Cannot read the plan output: { $error }
debug-explain-error = EXPLAIN failed: { $error }
debug-explain-sqlite-pool = The SQLite read pool is unavailable
debug-explain-sqlite-integer = A bound parameter is outside the SQLite integer range
debug-explain-query-plan-error = EXPLAIN QUERY PLAN failed: { $error }
debug-explain-sqlite-empty = SQLite returned no plan rows
debug-explain-sqlite-row = Unrecognized SQLite plan row: { $detail }
debug-explain-auto-index = { $table } (automatic temporary index, built on every run)
debug-explain-sqlite-scan = { $table } (SCAN, no index chosen)
debug-explain-rowid = { $table } → integer primary key (SEARCH)
debug-explain-temp-trees = { $summary }; temporary B-tree: { $trees }
debug-explain-no-table = No table read: a constant result, or the optimizer removed the table access
debug-explain-seek = Index lookup
debug-explain-index-scan = Index scan
debug-explain-scan = Full scan
debug-explain-access = { $kind }: { $description }
debug-explain-separator = {"; "}
debug-explain-mysql-empty = MySQL returned no plan rows
debug-explain-mysql-extra = MySQL reported no recognizable access type: { $extra }
debug-explain-mysql-kind = Unrecognized MySQL access type: { $kind }
debug-explain-index = { $table } → { $key } ({ $kind })
debug-explain-no-index = { $table } ({ $kind }, no index chosen)
# Why the SQL with parameters filled in cannot be shown.
debug-expand-backslash = The original SQL has backslash-escaped quotes, which depend on MySQL's sql_mode, so the parameters cannot be expanded reliably
debug-expand-float = A non-finite floating-point parameter cannot be written as a MySQL number literal
debug-expand-executable-comment = MySQL executable comments depend on the server version, so the parameters cannot be expanded reliably
debug-expand-numbered = Numbered question-mark parameters are not expanded
debug-expand-unused = Bound parameter { $n } is not used by the SQL
debug-expand-from-one = SQL parameter numbers must start at 1
debug-expand-missing = The SQL refers to bound parameter { $n }, which does not exist
debug-expand-unclosed-string = Unclosed SQL string or quoted identifier
debug-expand-unclosed-comment = Unclosed SQL block comment
debug-expand-not-observed = The bound parameters were not captured by request observation and cannot be shown reliably
debug-expand-encoding = Parameter encoding failed: { $error }
# Names of the caches in the panel (cache::registry).
debug-cache-settings = Site settings and navigation
debug-cache-policy = Policy documents
debug-cache-branding = Brand image bindings
debug-cache-credit-rules = { $credit } reward rules
debug-cache-links = Links (home page and directory)
debug-cache-pins = Site-wide level 2 and 3 pin index
debug-cache-moderators = Moderator roster
debug-cache-honors = Honor group directory
debug-cache-forums = Readable forums (visitors, members)
debug-cache-domains = Domain table of multi-domain sites
debug-cache-skins = Skins
debug-cache-statistics = Site statistics
debug-cache-scoped-statistics = Community statistics by scope (multi-domain sites)
debug-cache-recent-members = Newest members for visitors
debug-cache-forum-statistics = Forum statistics
debug-cache-thread-list = Thread lists for visitors (home, forums, search)
debug-cache-sidebar-topics = Sidebar hot and newest threads
debug-cache-forum-pins = Cross-forum pins on forum pages for visitors
debug-cache-feed-items = Feed items
debug-cache-floors = Floors for visitors
debug-cache-rendered-bodies = Rendered post bodies
debug-cache-member-frame = Member frame (reading preferences and forum filters)
debug-cache-data-threads = Data source thread lists
debug-cache-data-members = Data source member lists
debug-cache-thread-outline = Thread outlines
debug-cache-board-last-posts = Latest thread of each board
debug-cache-brand-images = Brand image bytes
debug-cache-display-images = Display versions of avatars and forum icons
debug-cache-thumbnails = Thread list thumbnails
debug-cache-storage-clients = Object storage clients (by storage profile)
