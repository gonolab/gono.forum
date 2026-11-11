## Relative times (clock.rs), used within the relative window of the site time settings.

time-just-now = just now
time-minutes-ago = { $n ->
    [one] { $n } minute ago
   *[other] { $n } minutes ago
}
time-hours-ago = { $n ->
    [one] { $n } hour ago
   *[other] { $n } hours ago
}

## Calendar days, for "last seen" on profiles.

time-today = today
time-yesterday = yesterday
time-days-ago = { $n ->
    [one] { $n } day ago
   *[other] { $n } days ago
}
