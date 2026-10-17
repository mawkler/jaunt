pragma Singleton

import QtQuick
import Quickshell

// The switcher's look: how big everything is, how long it takes to move, and
// the colours it is drawn in, which come from a theme.

Singleton {
    id: theme

    readonly property real panelRadius: 20
    readonly property real padTop: 18
    readonly property real titleInset: 4

    // Top to bottom: the heading line, the applications, and the sheet
    // holding the windows of the one being switched to.
    readonly property real headingGap: 18
    readonly property real barGap: 16

    // The sheet, and the tab joining it to the application being switched
    // to. It is the panel less this all round, with corners that nest inside
    // the panel's.
    readonly property real sheetInset: 20
    // The windows sit this far inside the sheet, and the heading lines up
    // with them.
    readonly property real sheetPadSide: 8
    readonly property real padSide: sheetInset + sheetPadSide
    readonly property real sheetRadius: 10
    // Under the keys at the bottom of the sheet. The room over the windows is
    // all of the room under them, keys included.
    readonly property real sheetPadBottom: 8
    // How far the tab reaches above its application's cell, and how round the
    // corners are where it meets the sheet.
    readonly property real tabRise: 6
    readonly property real fillet: 10
    // Between the windows and the keys that walk them.
    readonly property real hintGap: 6

    // The applications: a cell each, with a dot per open window under the
    // icon and the key that reaches it under the cell.
    readonly property real cellSize: 54
    readonly property real cellRadius: 15
    readonly property real cellGap: 6
    readonly property real iconSize: 34
    readonly property real dotSize: 4
    readonly property real dotGap: 3
    // At most this many dots, however many windows there are.
    readonly property int dots: 4
    // At most this many windows carry a number: as many as the daemon binds
    // digits for.
    readonly property int numbered: 9
    readonly property real keyTop: 6

    // The windows of the application being switched to.
    readonly property real cardRadius: 8
    // The room around a window, which the highlight fills.
    readonly property real cardPad: 8
    readonly property real cardGap: 4
    readonly property real titleTop: 6
    readonly property real highlightRadius: 14
    // How big an application's windows are while they're away: they grow to
    // full size out of its icon, and shrink back into it.
    readonly property real pageGrow: 0.92
    // How much of the next window shows past the highlighted one, when they
    // don't all fit.
    readonly property real peek: 40

    // The shadow the sheet casts onto the panel: how soft, and how far below
    // it.
    readonly property real sheetShadowBlur: 24
    readonly property real sheetShadowDrop: 6

    // An application with nothing open, which is there for its key.
    readonly property real closed: 0.35
    // An icon standing in for a window that has no picture yet.
    readonly property real standin: 0.35

    // Every movement takes this long, on the curve Hyprland moves its own
    // windows and layers on: its easeOutQuint, quick to set off and a long
    // while settling.
    readonly property int duration: 220
    readonly property var curve: [0.23, 1, 0.32, 1, 1, 1]
    // Except the highlight moving from one window to the next, which happens
    // as fast as the key is pressed and has to keep up with it.
    readonly property int cycle: 120

    // The six colours a theme sets, as the daemon sends them: the default
    // theme's until it does. Every colour below is worked out from these, so
    // that a theme needn't say what a border is, and a light one works too.
    property var palette: ({
            background: "#15171c",
            surface: "#20232b",
            accent: "#6b8cff",
            text: "#c4cad8",
            bright: "#eef1f7",
            muted: "#78819a"
        })

    // How see-through the panel and the sheet are is up to the
    // configuration: these are their colours alone.
    readonly property color panel: palette.background
    readonly property color sheetFill: palette.surface
    // Edges and keys are a touch of the brightest colour over whatever is
    // under them: lighter on a dark theme, darker on a light one.
    readonly property color panelEdge: withAlpha(palette.bright, 0.08)
    readonly property color sheetEdge: withAlpha(palette.bright, 0.07)
    readonly property color cardEdge: withAlpha(palette.bright, 0.1)
    readonly property color keycapEdge: withAlpha(palette.bright, 0.1)
    readonly property color keycapFill: withAlpha(palette.bright, 0.06)
    readonly property color heading: palette.bright
    readonly property color text: palette.text
    readonly property color selectedText: palette.bright
    readonly property color keycapText: palette.text
    // The window a switch would land on, the hints, and a window's dots.
    readonly property color muted: palette.muted
    // The window a switch would land on, and nothing else: the application is
    // marked by the tab, so that the two never look like the same kind of
    // thing.
    readonly property color selection: withAlpha(palette.accent, 0.22)
    readonly property color selectionEdge: withAlpha(mix(palette.accent, palette.bright, 0.22), 0.65)
    // The shadow the sheet casts onto the panel, fainter the lighter the
    // panel is: a dark shadow on a light panel reads as dirt.
    readonly property color sheetShadow: Qt.rgba(0, 0, 0, 0.11 - 0.06 * Qt.color(palette.background).hslLightness)
    // The dot of the window a switch would land on, and every other window's.
    readonly property color dotLit: palette.bright
    readonly property color dotDim: mix(palette.muted, palette.background, 0.28)
    // Behind a window's picture, until there is one.
    readonly property color cardFill: withAlpha(palette.background, 0.6)
    // Initials, for an application the icon theme has nothing for.
    readonly property color monogramFill: withAlpha(palette.muted, 0.2)

    function clamp(value, lowest, highest) {
        return Math.max(lowest, Math.min(highest, value));
    }

    // A colour with some of what is behind it showing through.
    function withAlpha(colour, alpha) {
        const solid = Qt.color(colour);

        return Qt.rgba(solid.r, solid.g, solid.b, alpha);
    }

    // A colour some of the way from one to another.
    function mix(from, to, amount) {
        const a = Qt.color(from);
        const b = Qt.color(to);

        return Qt.rgba(a.r + (b.r - a.r) * amount, a.g + (b.g - a.g) * amount, a.b + (b.b - a.b) * amount, a.a + (b.a - a.a) * amount);
    }
}
