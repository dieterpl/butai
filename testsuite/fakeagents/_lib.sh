# Shared driver for the scripted agent CLIs.
#
# butai decides whether an agent is working, waiting or idle by re-rendering its
# pane and scanning the bottom FOOTER_SCAN_ROWS (8) lines for marker strings.
# That means agent compatibility is a property of what a CLI *draws*, not of any
# protocol — so these fakes reproduce the drawing, and the real CLIs are only
# needed to confirm the strings have not changed upstream.
#
# An agent script sources this file, defines emit_banner/emit_busy/
# emit_question/emit_choice/emit_prose, and calls run_script. FAKE_SCRIPT drives
# the phases:
#
#   FAKE_SCRIPT="busy:6,question:600"     draw the busy marker, then a dialog
#   FAKE_SCRIPT="choice:600"              a multiple-choice question dialog
#   FAKE_SCRIPT="noisy:3,idle:600"        sustained output, then quiet
#   FAKE_SCRIPT="prose:8"                 the negative control
#   FAKE_SCRIPT="busy:2,exit:3"           die with a non-zero code
#
# Each phase draws ONCE and then waits in silence. That is deliberate: butai has
# two independent working signals — a footer marker and raw output recency — and
# an agent that redrew continuously would trip both, so a marker test could not
# tell which one fired. Going quiet after the draw isolates the marker.
#
# bash rather than sh, for the SIGWINCH handling below.

IDLE_LINE='> '
CURRENT_PHASE=""
LAST_SIZE=""

emit_banner() { :; }
emit_busy() { :; }
emit_question() { :; }
emit_choice() { :; }
emit_prose() { :; }

# Push what is already on screen off the top, so whatever is drawn next ends up
# at the BOTTOM of the pane.
#
# Not cosmetic: butai reads status from the last eight rendered rows, and a real
# agent's status line is at the bottom because its transcript fills the screen
# above it. A double that printed from the top would leave its marker near row 1
# and would be testing nothing.
scroll_to_bottom() {
    local rows i=0
    rows=$(tput lines 2>/dev/null || echo 40)
    while [ "$i" -lt "$rows" ]; do
        echo ""
        i=$((i + 1))
    done
}

# Repaint on resize, which is the whole reason this file needs bash.
#
# butai resizes every pane in a workspace whenever any client resizes, and on a
# grow the old content stays at the top with blank rows beneath it. A real TUI
# answers SIGWINCH with a full redraw and its status line lands at the bottom
# again; a double that did not would drift out of the footer band and report a
# detection failure that no real agent would hit.
draw() {
    case $CURRENT_PHASE in
        busy) scroll_to_bottom; emit_busy ;;
        question) scroll_to_bottom; emit_question ;;
        choice) scroll_to_bottom; emit_choice ;;
        prose) scroll_to_bottom; emit_prose ;;
        idle) clear_footer ;;
    esac
}

redraw() {
    # An agent whose "working" state is a scrolling transcript rather than a
    # status line has nothing to repaint, and repainting would keep its output
    # artificially recent — the very signal such a double exists to measure.
    # Those set REPAINTS=0. It suppresses repaints only: the first paint of each
    # phase always happens.
    [ "${REPAINTS:-1}" = 1 ] || return 0
    draw
}

# Scroll every marker out of the footer band, the way a real agent does when it
# finishes a turn and returns to a bare prompt.
clear_footer() {
    scroll_to_bottom
    printf '%s\n' "$IDLE_LINE"
}

# Sustained output with no marker anywhere — the fallback path, for agents whose
# status line butai does not recognise.
emit_noisy() {
    local seconds=$1 ticks i=0
    ticks=$((seconds * 5))
    while [ "$i" -lt "$ticks" ]; do
        printf 'reading src/lib.rs ... chunk %s\n' "$i"
        sleep 0.2
        i=$((i + 1))
    done
}

# Sleep in one-second steps, repainting whenever the pane's size has changed.
#
# The trap alone is not enough: butai creates a pane at a default size and
# resizes it to the stage almost immediately, and that first SIGWINCH can arrive
# before this script has installed its handler. A real agent survives that
# because it redraws constantly; these doubles draw once on purpose, so they
# have to notice the size themselves. Polling only *emits* on a change, so the
# output-recency signal stays clean.
nap() {
    local seconds=$1 i=0 now
    while [ "$i" -lt "$seconds" ]; do
        sleep 1
        i=$((i + 1))
        now="$(tput lines 2>/dev/null || echo 40)x$(tput cols 2>/dev/null || echo 80)"
        if [ "$now" != "$LAST_SIZE" ]; then
            LAST_SIZE=$now
            redraw
        fi
    done
}

run_script() {
    trap redraw WINCH
    emit_banner

    local script step phase secs
    script=${FAKE_SCRIPT:-busy:6,question:600}
    local IFS=','
    local steps=($script)
    unset IFS

    for step in "${steps[@]}"; do
        phase=${step%%:*}
        secs=${step#*:}
        if [ "$secs" = "$phase" ]; then
            secs=5
        fi
        CURRENT_PHASE=$phase
        case $phase in
            busy|question|choice|prose) draw; nap "$secs" ;;
            idle) clear_footer; nap "$secs" ;;
            noisy) emit_noisy "$secs" ;;
            bell) printf '\007'; nap "$secs" ;;
            exit) exit "$secs" ;;
            *) printf 'fake agent: unknown phase %s\n' "$phase"; nap "$secs" ;;
        esac
    done

    # Park, so the pane stays alive for the test to keep inspecting it.
    CURRENT_PHASE=""
    while :; do sleep 3600; done
}
