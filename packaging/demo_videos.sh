#!/bin/sh
# Demo videos: AI-vs-AI matches recorded headless, each made an MP4 (AV1/Opus) with its .okreplay beside it.
#   sh packaging/demo_videos.sh [options]
# Each match runs in a save folder of its own, made for it and removed after (OPENKUMITE_SAVE_DIR): the user's save is
# never touched. Options (default in brackets):
#   --mode M          arcade, vs, time-attack, survival, ball, force, tag-free, tag-teams, tag-arcade, levels [vs]
#   --fighters LIST   the fighters by name (paul,law; 4 for tag-free/tag-teams), or random: picked from the roster
#                     each match [random]
#   --seats LIST      the seats the AI plays [1,2; tag-free and tag-teams 1,2,3,4]
#   --styles S        one style for every seat, or one per seat (rushdown,thrower), or pick: one picked for each seat
#                     each match [pick]. Styles: classic rushdown defensive counter thrower evasive juggler mixed
#                     random (random: the game picks)
#   --difficulty D    easy, medium or hard [hard]
#   --level N         the AI's level, 0..9 [the game's own]
#   --matches N       how many matches [3]
#   --round-time S    a round's clock in seconds [the game's own]
#   --rounds N        the rounds to win [the game's own]
#   --size SIZE       720p, 1080p, 1440p, 4k or WxH [1080p]
#   --no-hud          the videos without the HUD
#   --out DIR         where the videos and replays go [demo_videos]
#   --assets DIR      the converted game data [$OPENKUMITE_ASSETS, else the save folder's assets/]
#   --bin FILE        the game [target/release/openkumite, built when missing]
#   --frames N        the most steps a match may take before it counts as failed [from the clock and rounds]
#   --no-ai           without the --ai flags: P1 idle, P2 the CPU (arcade, vs); to try the script on a game
#                     without them
# Needs the game's --ai, --ai-style, --ai-difficulty and --ai-level (unless --no-ai).
set -u
here="$(cd "$(dirname "$0")/.." && pwd)"

die() { printf 'demo_videos: %s\n' "$*" >&2; exit 2; }

mode=vs fighters=random seats='' styles=pick difficulty=hard level='' matches=3 round_time='' rounds='' size=1080p hud=1
out=demo_videos assets="${OPENKUMITE_ASSETS:-}" bin='' frames='' ai=1
while [ $# -gt 0 ]; do
    case "$1" in
    --no-hud) hud=0; shift; continue ;;
    --no-ai) ai=0; shift; continue ;;
    -h | --help) sed -n '2,/^set -u/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    esac
    [ $# -ge 2 ] || die "$1: needs a value (--help)"
    case "$1" in
    --mode) mode=$2 ;;
    --fighters) fighters=$2 ;;
    --seats) seats=$2 ;;
    --styles) styles=$2 ;;
    --difficulty) difficulty=$2 ;;
    --level) level=$2 ;;
    --matches) matches=$2 ;;
    --round-time) round_time=$2 ;;
    --rounds) rounds=$2 ;;
    --size) size=$2 ;;
    --out) out=$2 ;;
    --assets) assets=$2 ;;
    --bin) bin=$2 ;;
    --frames) frames=$2 ;;
    *) die "unknown option $1 (--help)" ;;
    esac
    shift 2
done

number() { case "$2" in '' | *[!0-9]*) die "$1 $2: a number" ;; esac; }
number --matches "$matches"
[ -z "$round_time" ] || number --round-time "$round_time"
[ -z "$rounds" ] || number --rounds "$rounds"
[ -z "$frames" ] || number --frames "$frames"
[ -z "$level" ] || { number --level "$level"; [ "$level" -le 9 ] || die "--level $level: 0..9"; }
case "$difficulty" in easy | medium | hard) ;; *) die "--difficulty $difficulty: easy, medium or hard" ;; esac
case "$mode" in
arcade | vs | time-attack | survival | ball | force | tag-free | tag-teams | tag-arcade | levels) ;;
*) die "--mode $mode: arcade, vs, time-attack, survival, ball, force, tag-free, tag-teams, tag-arcade or levels" ;;
esac
case "$size" in
720p) size=1280x720 ;;
1080p) size=1920x1080 ;;
1440p) size=2560x1440 ;;
4k | 4K) size=3840x2160 ;;
*[!0-9x]* | x* | *x | *x*x*) die "--size $size: 720p, 1080p, 1440p, 4k or WxH" ;;
*x*) ;;
*) die "--size $size: 720p, 1080p, 1440p, 4k or WxH" ;;
esac
case "$mode" in tag-free | tag-teams) people=4 ;; *) people=2 ;; esac
[ -n "$seats" ] || seats=$( [ "$people" = 4 ] && echo 1,2,3,4 || echo 1,2)
styles_known=" classic rushdown defensive counter thrower evasive juggler mixed random "
picks="classic rushdown defensive counter thrower evasive juggler mixed"
if [ "$styles" != pick ]; then
    for s in $(echo "$styles" | tr ',' ' '); do
        case "$styles_known" in *" $s "*) ;; *) die "--styles: no style $s (classic rushdown defensive counter thrower evasive juggler mixed random, or pick)" ;; esac
    done
fi

# the game data: as the game finds it (OPENKUMITE_ASSETS, else assets/ in the system's save folder)
if [ -z "$assets" ]; then
    case "$(uname -s)" in
    Darwin) assets="$HOME/Library/Application Support/OpenKumite/assets" ;;
    MINGW* | MSYS* | CYGWIN*) assets="${APPDATA:-$HOME/AppData/Roaming}/OpenKumite/assets" ;;
    *) assets="${XDG_DATA_HOME:-$HOME/.local/share}/openkumite/assets" ;;
    esac
fi
[ -f "$assets/manifest.json" ] || die "no game data in $assets (--assets DIR, or OPENKUMITE_ASSETS)"

if [ -z "$bin" ]; then
    bin="$here/target/release/openkumite"
    if [ ! -x "$bin" ]; then
        printf '== building %s\n' "$bin"
        (cd "$here" && cargo build --release -p openkumite) || die "the build failed"
    fi
fi
[ -x "$bin" ] || die "no game at $bin (--bin FILE)"

mkdir -p "$out" || die "cannot make $out"
out="$(cd "$out" && pwd)"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/openkumite_demo.XXXXXX")" || die "no temporary folder"
pid=
trap 'if [ -n "$pid" ]; then kill "$pid" 2>/dev/null; fi; rm -rf "$tmp"' EXIT
trap 'exit 130' INT TERM

# a random number below $1
below() { echo $(($(od -An -N4 -tu4 /dev/urandom | tr -d ' ') % $1)); }
# the $2-th word of $1 (from 1)
word() { echo "$1" | tr ' ' '\n' | sed -n "${2}p"; }

# the roster's names, as the game takes them: from the error it gives a name it does not know
roster=
if [ "$fighters" = random ]; then
    roster=$("$bin" --assets "$assets" --mode vs --player '?' --shot "$tmp/names.png" 2>&1 | sed -n 's/.*(names: \(.*\)).*/\1/p' | tail -n 1 |
        tr ',' '\n' | sed 's/^ *//; s/ *$//' | grep -v '^$' | sort -u | tr '\n' ' ')
    [ -n "$roster" ] || die "could not read the roster's names from $bin (give --fighters)"
fi
nroster=$(echo "$roster" | wc -w | tr -d ' ')

# the most steps a match may take: every round to its clock, with the intros, KOs and win poses
if [ -z "$frames" ]; then
    rt=${round_time:-99} rw=${rounds:-3}
    frames=$(((rt * 60 + 900) * (2 * rw + 1) + 3600))
fi

ok=0 failed=0 report=
i=0
while [ "$i" -lt "$matches" ]; do
    i=$((i + 1))
    tag=$(printf '%03d' "$i")
    # the fighters
    if [ "$fighters" = random ]; then
        list='' k=0
        while [ "$k" -lt "$people" ]; do
            k=$((k + 1))
            list="$list${list:+,}$(word "$roster" "$(($(below "$nroster") + 1))")"
        done
    else
        list=$fighters
    fi
    # the styles: per seat, as the game takes them (1:rushdown,2:thrower) and for the name (rushdown-thrower)
    style_arg='' style_name=''
    if [ "$ai" = 1 ]; then
        n=0
        for seat in $(echo "$seats" | tr ',' ' '); do
            n=$((n + 1))
            case "$styles" in
            pick) s=$(word "$picks" "$(($(below 8) + 1))") ;;
            *,*) s=$(echo "$styles" | cut -d, -f"$n"); [ -n "$s" ] || s=$(echo "$styles" | awk -F, '{ print $NF }') ;;
            *) s=$styles ;;
            esac
            style_arg="$style_arg${style_arg:+,}$seat:$s"
            style_name="$style_name${style_name:+-}$s"
        done
    else
        style_name=cpu
    fi
    who=$(echo "$list" | tr '[:upper:]' '[:lower:]' | sed 's/[^a-z0-9,]/_/g')
    case "$who" in *,*,*) who=$(echo "$who" | tr ',' '-') ;; *) who=$(echo "$who" | sed 's/,/-vs-/') ;; esac
    base="${tag}_${mode}_${who}_${style_name}"
    printf '\n== match %d of %d: %s, %s, %s\n' "$i" "$matches" "$mode" "$list" "$([ "$ai" = 1 ] && echo "$style_arg ($difficulty${level:+, level $level})" || echo "P1 idle, P2 the CPU")"

    # record it headless in a save folder of its own, until its replay is saved
    save="$tmp/save_$tag"
    mkdir -p "$save"
    log="$save/match.log"
    set -- --assets "$assets" --mode "$mode" --fighters "$list"
    [ -z "$round_time" ] || set -- "$@" --round-time "$round_time"
    [ -z "$rounds" ] || set -- "$@" --rounds "$rounds"
    if [ "$ai" = 1 ]; then
        set -- "$@" --ai "$seats" --ai-style "$style_arg" --ai-difficulty "$difficulty"
        # (Force: a second AI seat is its co-op partner)
        case "$mode,$seats" in force,*2*) set -- "$@" --coop ;; esac
        [ -z "$level" ] || set -- "$@" --ai-level "$level"
    elif [ "$mode" = vs ] || [ "$mode" = tag-arcade ]; then
        set -- "$@" --p2-cpu
    fi
    set -- "$@" --shot "$save/end.png" --frames "$frames"
    OPENKUMITE_SAVE_DIR="$save" RUST_LOG=info "$bin" "$@" >/dev/null 2>"$log" &
    pid=$!
    t=0 id=
    while kill -0 "$pid" 2>/dev/null; do
        id=$(sed -n 's/.*replay \([^ ]*\) saved: .*/\1/p' "$log" | head -n 1)
        [ -z "$id" ] || break
        [ ! -t 1 ] || printf '\r   recording: %ds' "$t"
        sleep 1
        t=$((t + 1))
    done
    # (its replay saved: the rest of the run is not wanted)
    if [ -n "$id" ]; then kill "$pid" 2>/dev/null; fi
    wait "$pid" 2>/dev/null
    status=$?
    pid=
    [ ! -t 1 ] || printf '\r'
    [ -n "$id" ] || id=$(sed -n 's/.*replay \([^ ]*\) saved: .*/\1/p' "$log" | head -n 1)
    if [ -z "$id" ] || [ ! -f "$save/replays/$id.okreplay" ]; then
        why=$(grep -E 'not saved|Error|ERROR|error:' "$log" | head -n 3)
        printf '   FAILED: no replay saved (%s)\n' "$([ "$status" = 0 ] && echo "$frames steps run" || echo "the game exited with $status")"
        [ -z "$why" ] || printf '   %s\n' "$why"
        cp "$log" "$out/$base.log" 2>/dev/null && printf '   the log: %s\n' "$out/$base.log"
        failed=$((failed + 1)) report="$report\n  FAILED  $base: no replay"
        rm -rf "$save"
        continue
    fi
    cp "$save/replays/$id.okreplay" "$out/$base.okreplay"
    printf '   recorded in %ds: replay %s\n' "$t" "$(sed -n 's/.*replay [^ ]* saved: \(.*\)/\1/p' "$log" | head -n 1)"

    # the video
    set -- --assets "$assets" --export-video "$id" --out "$out/$base.mp4" --size "$size"
    [ "$hud" = 1 ] || set -- "$@" --no-hud
    vlog="$save/video.log"
    started=$(date +%s)
    OPENKUMITE_SAVE_DIR="$save" RUST_LOG=info "$bin" "$@" 2>"$vlog" | {
        done_=0
        while read -r what a b; do
            case "$what" in
            PROGRESS) [ ! -t 1 ] || [ "${b:-0}" -le 0 ] || printf '\r   video: %d%%' $((a * 100 / b)) ;;
            DONE) done_=1 ;;
            esac
        done
        [ "$done_" = 1 ]
    }
    done_=$?
    [ ! -t 1 ] || printf '\r'
    secs=$(($(date +%s) - started))
    if [ "$done_" = 0 ] && [ -s "$out/$base.mp4" ]; then
        bytes=$(wc -c <"$out/$base.mp4" | tr -d ' ')
        printf '   video in %ds: %s (%d KB)\n' "$secs" "$out/$base.mp4" $((bytes / 1024))
        ok=$((ok + 1)) report="$report\n  ok      $base.mp4 ($((bytes / 1024)) KB)"
    else
        printf '   FAILED: the video export did not finish (no DONE)\n'
        grep -E 'Error|ERROR|error:' "$vlog" | head -n 3 | sed 's/^/   /'
        cp "$vlog" "$out/$base.video.log" 2>/dev/null && printf '   the log: %s\n' "$out/$base.video.log"
        rm -f "$out/$base.mp4"
        failed=$((failed + 1)) report="$report\n  FAILED  $base: no video (the replay is kept)"
    fi
    rm -rf "$save"
done

printf '\n== %d of %d made, %d failed, in %s' "$ok" "$matches" "$failed" "$out"
printf '%b\n' "$report"
[ "$failed" = 0 ]
