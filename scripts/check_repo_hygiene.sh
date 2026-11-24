#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C.UTF-8

cd -- "$(git rev-parse --show-toplevel)"

reject() {
    printf 'Repository hygiene: %s\n' "$*" >&2
    return 1
}

check_path() {
    local path
    path=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
    case "$path" in
        */agents.md|agents.md|*/claude.md|claude.md|*/gemini.md|gemini.md|\
        docs/internal/*|scripts/ingest_cms.py|*.zip|*.bin|.cache/*|\
        *.local.md|*/.claude/*|.claude/*|*/.codex/*|.codex/*|\
        memory-bank/*|*/memory-bank/*|sandbox/vendor/*|artifacts/*|target/*|dist/*|\
        *.tar|*.tar.gz|*.tgz|*.jar|*.whl|*.db|*.sqlite)
            reject "forbidden path $1" ;;
    esac
}

check_text() {
    local label=$1
    python -c '
import re
import sys

content = sys.stdin.buffer.read().decode("utf-8", errors="replace")
unfinished = re.search(r"(?<![\w-])TO[D]O(?![\w.-])", content, re.IGNORECASE)
emoji = re.search(
    r"[\u20e3\u2600-\u27bf\U0001f1e6-\U0001f1ff\U0001f300-\U0001faff]"
    r"|[\u00a9\u00ae\u203c\u2049\u2122\u2139\u2194-\u2199\u2300-\u23ff]\ufe0f",
    content,
)
if unfinished or emoji:
    sys.exit(f"Repository hygiene: emoji or unfinished marker in {sys.argv[1]}")
' "$label"
}

check_message() {
    local trailers
    trailers=$(git interpret-trailers --parse)
    if grep -qi '^Co-Authored-By[[:space:]]*:' <<< "$trailers"; then
        reject "forbidden commit trailer"
    fi
}

check_secrets() {
    local version
    version=$(gitleaks version)
    if [[ "$version" != 8.30.1 ]]; then
        reject "gitleaks 8.30.1 required, found $version"
    fi
    gitleaks git --pre-commit --redact --staged --verbose
}

check_index() {
    local path
    while IFS= read -r -d '' path; do
        check_path "$path"
    done < <(git ls-files -z)
    while IFS= read -r -d '' path; do
        git cat-file blob ":$path" | check_text "$path"
    done < <(git diff --cached --name-only --diff-filter=ACMRT -z)
}

check_candidate() {
    local path
    while IFS= read -r -d '' path; do
        check_path "$path"
        if [[ -f "$path" ]]; then
            check_text "$path" < "$path"
        fi
    done < <(git ls-files --cached --others --exclude-standard -z)
}

check_history() {
    local commit row path oid
    local -A scanned=()
    while IFS= read -r commit; do
        git show -s --format=%B "$commit" | check_message
        while IFS= read -r -d '' row; do
            path=${row#*$'\t'}
            check_path "$path"
            if [[ "$row" =~ ^[0-7]+[[:space:]]blob[[:space:]]([0-9a-f]+)$'\t' ]]; then
                oid=${BASH_REMATCH[1]}
                if [[ ! -v scanned[$oid] ]]; then
                    git cat-file blob "$oid" | check_text "$commit:$path"
                    scanned[$oid]=1
                fi
            fi
        done < <(git ls-tree -rz "$commit")
    done < <(git rev-list --all)
}

self_test() (
    local fixture=$1
    script="$PWD/scripts/check_repo_hygiene.sh"
    unset GIT_INDEX_FILE GIT_DIR GIT_WORK_TREE
    cd -- "$fixture"
    if [[ -n "$(ls -A)" ]]; then
        reject "self-test fixture directory is not empty"
    fi
    git init -q
    git config core.autocrlf false
    printf 'Working change\n' > message
    bash "$script" --commit-msg message
    printf '\nco-authored-by: Someone <somebody@example.org>\n' >> message
    if bash "$script" --commit-msg message; then
        reject "forbidden trailer was accepted"
    fi
    printf 'AGENTS.md\n' >> .git/info/exclude
    printf 'internal\n' > AGENTS.md
    git add -f -- AGENTS.md
    if bash "$script" --index; then
        reject "force-added instruction file was accepted"
    fi
    git rm -q --cached AGENTS.md
    local forbidden
    # Agent notes, internal documents, fetched vendor trees, raw archives and run reports
    # must never reach a commit, even when force-added past .gitignore.
    for forbidden in memory-bank/activeContext.md docs/internal/PLAN.md \
        sandbox/vendor/davinci-pas-server/go/main.go data/raw/cms-ptp.zip \
        sandbox/vendor/synthea/synthea-with-dependencies.jar artifacts/inferno/pas.json \
        scripts/ingest_cms.py; do
        mkdir -p "$(dirname "$forbidden")"
        printf 'fixture\n' > "$forbidden"
        if bash "$script" --candidate; then
            reject "candidate file $forbidden was accepted"
        fi
        git add -f -- "$forbidden"
        if bash "$script" --index; then
            reject "force-added $forbidden was accepted"
        fi
        git rm -q --cached -- "$forbidden"
        rm -- "$forbidden"
    done
    printf 'clean\n' > clean.txt
    git add -- clean.txt
    printf 'TO%s\n' 'DO' > clean.txt
    bash "$script" --index
    if bash "$script" --candidate; then
        reject "unfinished candidate file was accepted"
    fi
    git add -- clean.txt
    printf 'clean\n' > clean.txt
    if bash "$script" --index; then
        reject "unfinished staged blob was accepted"
    fi
    printf '\U0001f600\n' > clean.txt
    if bash "$script" --candidate; then
        reject "emoji in candidate file was accepted"
    fi
    printf 'clean\n' > clean.txt
    git add -- clean.txt
    bash "$script" --index
    printf 'Repository hygiene self-test passed.\n'
)

while (($#)); do
    case "$1" in
        --candidate) check_candidate ;;
        --index) check_index ;;
        --history) check_history ;;
        --self-test)
            python -c '
import subprocess
import tempfile

with tempfile.TemporaryDirectory(prefix="medverify-hygiene-") as fixture:
    subprocess.run(
        ["bash", "scripts/check_repo_hygiene.sh", "--fixture", fixture],
        check=True,
    )
' ;;
        --fixture) self_test "$2"; shift ;;
        --gitleaks) check_secrets ;;
        --commit-msg) check_message < "$2"; shift ;;
        *) reject "unknown argument $1" ;;
    esac
    shift
done
