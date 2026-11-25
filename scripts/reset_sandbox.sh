#!/usr/bin/env bash
# Snapshot or restore the sandbox HAPI database and reset payer state.
#
#   bash scripts/reset_sandbox.sh --create-template   snapshot the seeded database
#   bash scripts/reset_sandbox.sh                      restore it and restart the payer servers
#
# HAPI is the only writer to the database. It is stopped and every remaining connection is
# terminated before PostgreSQL copies a database, because CREATE DATABASE ... TEMPLATE fails
# or copies a torn state while sessions are open. Only the hapi database and its snapshot are
# touched; Inferno state and the other volumes stay as they are. The payer servers keep their
# state in memory, so stopping and starting them is the payer reset.
#
# Every step can be repeated after an interruption. A new snapshot is built under a temporary
# name and swapped in only when complete, so the previous good snapshot survives a failed
# refresh. A restore copies the snapshot into a staging database before the live one is
# dropped, then renames it into place; a retry after any interruption starts that copy again.
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.."

project=${COMPOSE_PROJECT_NAME:-medverify-sandbox}
compose=(docker compose -p "$project" -f sandbox/docker-compose.yaml)
database=hapi
template=hapi_template
writers=(hapi)
payers=(crd dtr pas)
all_databases="'$database', '$template', '${template}_next', '${template}_prev', '${database}_restore'"

sql() {
    local line
    printf '%s;\n\\echo medverify-query-done\n' "$1" >&"$sql_input"
    while IFS= read -r -u "$sql_output" line; do
        if [[ "$line" == medverify-query-done ]]; then
            return 0
        fi
        printf '%s\n' "$line"
    done
    printf 'reset_sandbox: PostgreSQL session closed during a query\n' >&2
    return 1
}

exists() {
    [[ "$(sql "SELECT count(*) FROM pg_database WHERE datname = '$1';")" == 1 ]]
}

drop() {
    if exists "$1"; then
        sql "ALTER DATABASE $1 WITH IS_TEMPLATE false;"
        sql "DROP DATABASE $1;"
    fi
}

drain() {
    local remaining attempt
    for attempt in $(seq 1 30); do
        sql "SELECT pg_terminate_backend(pid) FROM pg_stat_activity
             WHERE datname IN ($all_databases) AND pid <> pg_backend_pid();" >/dev/null
        remaining=$(sql "SELECT count(*) FROM pg_stat_activity WHERE datname IN ($all_databases);")
        if [[ "$remaining" == 0 ]]; then
            return 0
        fi
        sleep 1
    done
    printf 'reset_sandbox: %s connections still open after draining\n' "$remaining" >&2
    return 1
}

# Test-only fault injection: MEDVERIFY_RESET_INTERRUPT_AT=<step> kills this process at that
# step the way a lost terminal would, so the retry path can be exercised for real.
checkpoint() {
    if [[ "${MEDVERIFY_RESET_INTERRUPT_AT:-}" == "$1" ]]; then
        printf 'reset_sandbox: interrupted at %s\n' "$1" >&2
        kill -KILL $$
    fi
}

# Finish or roll back a snapshot swap that an earlier run left half done.
recover_template() {
    if ! exists "$template" && exists "${template}_prev"; then
        sql "ALTER DATABASE ${template}_prev RENAME TO $template;"
        printf 'reset_sandbox: restored the previous snapshot after an interrupted refresh\n' >&2
    fi
    # An interruption can leave the snapshot renamed but still open to connections.
    if exists "$template"; then
        sql "ALTER DATABASE $template WITH IS_TEMPLATE true ALLOW_CONNECTIONS false;"
    fi
    drop "${template}_prev"
    drop "${template}_next"
}

mode=restore
case "${1:-}" in
    "") ;;
    --create-template) mode=template ;;
    *) printf 'usage: %s [--create-template]\n' "$0" >&2; exit 2 ;;
esac

# The same PostgreSQL session holds the lock and runs every query. Other checkouts reach
# the same lock, and PostgreSQL releases it when the session closes after an interruption.
coproc RESET_SQL {
    exec "${compose[@]}" exec -T postgres psql -v ON_ERROR_STOP=1 -X -q -A -t -U hapi -d postgres
}
# Duplicate the coprocess descriptors so command substitutions in sql() can use them.
exec {sql_input}>&"${RESET_SQL[1]}" {sql_output}<&"${RESET_SQL[0]}"
lock_acquired=$(sql "SELECT pg_try_advisory_lock(1297502790, 1)")
if [[ "$lock_acquired" != t ]]; then
    printf 'reset_sandbox: another snapshot or restore holds the PostgreSQL lock\n' >&2
    exit 75
fi

started=$SECONDS
"${compose[@]}" stop "${writers[@]}" "${payers[@]}"
drain
recover_template

if [[ "$mode" == template ]]; then
    if ! exists "$database"; then
        printf 'reset_sandbox: no %s database to snapshot\n' "$database" >&2
        exit 1
    fi
    sql "CREATE DATABASE ${template}_next WITH TEMPLATE $database OWNER hapi;"
    checkpoint snapshot-copied
    if exists "$template"; then
        sql "ALTER DATABASE $template WITH IS_TEMPLATE false;"
        sql "ALTER DATABASE $template RENAME TO ${template}_prev;"
    fi
    checkpoint snapshot-swapping
    sql "ALTER DATABASE ${template}_next RENAME TO $template;"
    # Refusing connections keeps the snapshot byte-identical between restores.
    sql "ALTER DATABASE $template WITH IS_TEMPLATE true ALLOW_CONNECTIONS false;"
    drop "${template}_prev"
else
    if ! exists "$template"; then
        printf 'reset_sandbox: no %s snapshot; run with --create-template after seeding\n' "$template" >&2
        exit 1
    fi
    drop "${database}_restore"
    sql "CREATE DATABASE ${database}_restore WITH TEMPLATE $template OWNER hapi;"
    checkpoint restore-copied
    drop "$database"
    checkpoint live-dropped
    sql "ALTER DATABASE ${database}_restore RENAME TO $database;"
fi

"${compose[@]}" up -d --wait "${writers[@]}" "${payers[@]}"
printf 'reset_sandbox: %s finished in %d s\n' "$mode" "$((SECONDS - started))"
