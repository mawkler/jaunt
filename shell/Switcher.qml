// The panel: what the switch is for, then the applications, then a sheet with
// the windows of the one being switched to.
//
// Choosing is two steps, an application and then one of its windows, and the
// panel is drawn to say so: the application sits in a tab that the sheet of
// its windows hangs from, and only the window is marked in colour.
//
// It is one size for as long as the switcher is open. Switching to another
// application slides the tab along the applications, and its windows grow out
// from under it as the last one's go back; nothing changes size.

pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Layouts

Item {
    id: switcher

    required property var settings
    required property var scene
    required property string selected
    required property string subject
    required property bool capturing
    required property bool animate
    required property int screenWidth
    required property int screenHeight

    readonly property string family: settings.font ?? ""
    // The font keys are written in, so that every key is as wide as every
    // other. The desktop's own monospaced font when it is installed, and
    // otherwise whatever the system calls monospace, which always is.
    readonly property string mono: {
        const wanted = settings.mono ?? "";

        return wanted !== "" && Qt.fontFamilies().includes(wanted) ? wanted : "monospace";
    }
    readonly property int contentWidth: resolve(settings.width, screenWidth)
    // How solid the panel and the sheet on it are, as configured.
    readonly property real backgroundOpacity: settings.backgroundOpacity ?? 1
    readonly property real foregroundOpacity: settings.foregroundOpacity ?? 1
    readonly property int maxHeight: resolve(settings.maxHeight, screenHeight)

    // The room on the sheet under the windows, with the keys that walk them,
    // and as much again over them, so that they sit in the middle of it.
    readonly property real sheetRoom: Theme.hintGap + hints.height + Theme.sheetPadBottom
    // Everything in the panel but the windows.
    readonly property real chrome: Theme.padTop + title.implicitHeight + Theme.headingGap + bar.implicitHeight + Theme.barGap + 2 * sheetRoom + Theme.sheetInset
    // The room a window's title takes under its picture, which has to hold
    // the key beside it as well, and does so whether or not there is one: the
    // windows are the same size either way.
    readonly property real titleSpace: Theme.titleTop + Math.max(titles.height, keySize.height)
    // How tall a window's picture is: as tall as configured, unless that
    // would make the panel taller than it may be.
    readonly property int tall: Math.max(48, Math.min(settings.previewHeight, maxHeight - chrome - 2 * Theme.cardPad - titleSpace))

    // The window is exactly as big as the panel.
    readonly property int surfaceWidth: Math.min(screenWidth, panel.width)
    readonly property int surfaceHeight: Math.min(screenHeight, panel.height)

    // The open applications by the app id they are known by, and the order
    // they go in, which only changes when the applications do: a switch moves
    // things along rather than building them again.
    property var openIds: []
    property var rows: ({})
    property var closed: []

    // Which of the switch's application's windows it would land on.
    readonly property int selectedIndex: (rows[scene?.target ?? ""]?.windows ?? []).findIndex(window => window.id === selected)

    onSceneChanged: {
        if (!scene)
            return;

        const ids = scene.rows.map(row => row.appId);

        if (!same(ids, openIds))
            openIds = ids;

        const byId = {};

        for (const row of scene.rows)
            byId[row.appId] = row;

        rows = byId;

        const keys = scene.absent.map(chip => chip.key + "\n" + chip.name);

        if (!same(keys, closed.map(chip => chip.key + "\n" + chip.name)))
            closed = scene.absent;
    }

    function same(these, those) {
        if (these.length !== those.length)
            return false;

        for (let i = 0; i < these.length; i++) {
            if (these[i] !== those[i])
                return false;
        }

        return true;
    }

    function resolve(length, screen) {
        if (!length)
            return 0;

        if (length.pixels !== undefined)
            return length.pixels;

        return Math.round(screen * length.portion);
    }

    FontMetrics {
        id: titles

        font.family: switcher.family
        font.pixelSize: 12
    }

    // As big as a key, so that the room left for one is exactly a key's.
    Keycap {
        id: keySize

        visible: false
        text: "1"
        family: switcher.mono
    }

    Rectangle {
        id: panel

        anchors.centerIn: parent
        width: switcher.contentWidth + 2 * Theme.padSide
        height: hints.y + hints.height + Theme.sheetPadBottom + Theme.sheetInset
        radius: Theme.panelRadius
        color: Theme.withAlpha(Theme.panel, switcher.backgroundOpacity)
        border.width: 1
        border.color: Theme.panelEdge

        // Where the switch is, as a path: the application, then the window
        // it would land on. The key that cancels it sits at the end of the
        // line, where the eye passes over it.
        RowLayout {
            id: title

            x: Theme.padSide + Theme.titleInset
            y: Theme.padTop
            width: switcher.contentWidth - 2 * Theme.titleInset
            spacing: 8

            Text {
                text: switcher.scene?.name ?? ""
                color: Theme.heading
                font.family: switcher.family
                font.pixelSize: 15
                font.weight: Font.DemiBold
            }

            Text {
                text: "›"
                color: Theme.muted
                font.family: switcher.family
                font.pixelSize: 15
            }

            Text {
                Layout.fillWidth: true
                Layout.preferredWidth: 0
                text: switcher.subject
                color: Theme.muted
                font.family: switcher.family
                font.pixelSize: 13
                elide: Text.ElideRight
            }

            Hint {
                Layout.leftMargin: 6
                key: switcher.settings.cancelKey
                says: "cancel"
                family: switcher.family
                mono: switcher.mono
            }
        }

        // The application being switched to sits in a tab, and its windows
        // on a sheet that the tab is part of. Behind the applications, so
        // that the tab is too.
        Sheet {
            anchors.fill: parent
            sheetX: Theme.sheetInset
            sheetY: bar.y + bar.height + Theme.barGap
            sheetWidth: panel.width - 2 * Theme.sheetInset
            sheetHeight: hints.y + hints.height + Theme.sheetPadBottom - sheetY
            tabX: bar.x + bar.tabX - Theme.cellGap / 2
            tabWidth: Theme.cellSize + Theme.cellGap
            tabTop: bar.y - Theme.tabRise
            tabbed: bar.tabbed
            opacity: switcher.foregroundOpacity
        }

        AppBar {
            id: bar

            anchors.horizontalCenter: parent.horizontalCenter
            y: Theme.padTop + title.implicitHeight + Theme.headingGap
            openIds: switcher.openIds
            rows: switcher.rows
            closed: switcher.closed
            current: switcher.scene?.target ?? ""
            selectedIndex: switcher.selectedIndex
            icons: switcher.settings.icons
            animate: switcher.animate
            family: switcher.family
            mono: switcher.mono
        }

        // The windows, an application's at a time. Every open application's
        // windows wait here, one on top of the other and all but one hidden,
        // so that switching never has to build them again.
        Item {
            id: strip

            x: Theme.padSide
            y: bar.y + bar.height + Theme.barGap + switcher.sheetRoom
            width: switcher.contentWidth
            height: 2 * Theme.cardPad + switcher.tall + switcher.titleSpace
            clip: true

            Repeater {
                model: switcher.openIds

                WindowPage {
                    id: windowPage

                    required property string modelData

                    // How far in this application's windows are: all the way
                    // while the switch is on it, and gone otherwise. Hidden,
                    // not taken away, so that they are still captured.
                    property real shown: current ? 1 : 0

                    Behavior on shown {
                        enabled: switcher.animate

                        NumberAnimation {
                            duration: Theme.duration
                            easing.type: Easing.BezierSpline
                            easing.bezierCurve: Theme.curve
                        }
                    }

                    width: strip.width
                    height: strip.height
                    // Squared, so the windows going are nearly gone before
                    // the ones coming are much there, rather than the two
                    // showing through each other.
                    opacity: shown * shown
                    // They come out of the application's own icon and go back
                    // into it, so they move a little towards it rather than
                    // across the panel.
                    transform: Scale {
                        origin.x: bar.x + bar.centreOf(windowPage.modelData) - strip.x
                        origin.y: 0
                        xScale: Theme.pageGrow + (1 - Theme.pageGrow) * windowPage.shown
                        yScale: xScale
                    }

                    windows: switcher.rows[modelData]?.windows ?? []
                    icons: switcher.rows[modelData]?.icons ?? []
                    current: switcher.scene?.target === modelData
                    selected: switcher.selected
                    tall: switcher.tall
                    previews: switcher.settings.previews
                    capturing: switcher.capturing
                    animate: switcher.animate
                    family: switcher.family
                    mono: switcher.mono
                }
            }
        }

        // The keys that walk the windows, with the windows: the application's
        // own key, and Shift with it the other way, when there is more than
        // one window to walk. Then Ctrl with it, for a window it doesn't have
        // yet, and the key that closes one, if there is one. Kept in place
        // without one, so that the panel stays the same size.
        Row {
            id: hints

            readonly property string key: switcher.scene?.cycleKey ?? ""
            readonly property bool several: (switcher.rows[switcher.scene?.target ?? ""]?.windows.length ?? 0) > 1

            x: strip.x + strip.width - width - Theme.cardPad
            y: strip.y + strip.height + Theme.hintGap
            height: implicitHeight
            spacing: 12
            opacity: key !== "" ? 1 : 0

            Hint {
                visible: hints.several
                key: hints.key || " "
                says: "next"
                family: switcher.family
                mono: switcher.mono
            }

            Hint {
                visible: hints.several
                key: "Shift + " + (hints.key || " ")
                says: "previous"
                family: switcher.family
                mono: switcher.mono
            }

            Hint {
                key: "Ctrl + " + (hints.key || " ")
                says: "new"
                family: switcher.family
                mono: switcher.mono
            }

            Hint {
                visible: key !== ""
                key: switcher.settings.closeKey ?? ""
                says: "close"
                family: switcher.family
                mono: switcher.mono
            }
        }
    }

    // One key and what it does.
    component Hint: Row {
        id: hint

        property string key
        property string says
        property string family
        property string mono

        spacing: 5

        Keycap {
            anchors.verticalCenter: parent.verticalCenter
            text: hint.key
            family: hint.mono
        }

        Text {
            anchors.verticalCenter: parent.verticalCenter
            text: hint.says
            color: Theme.muted
            font.family: hint.family
            font.pixelSize: 11
        }
    }
}
