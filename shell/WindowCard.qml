// One window: what it looks like right now, and what it is called.

pragma ComponentBehavior: Bound

import QtQuick
import Quickshell.Hyprland
import Quickshell.Wayland
import Quickshell.Widgets

Item {
    id: card

    required property var window
    // The application's icon, for a window that has no picture yet.
    required property var icons
    required property bool selected
    // Whether its application is the one being switched to.
    required property bool current
    required property int tall
    required property bool previews
    required property bool capturing
    required property bool animate
    required property string family
    required property string mono
    // The key that reaches this window, where it has one: its place in the
    // application's windows, from 1. Zero where there is nothing to choose
    // between, or past the last key there is.
    required property int number

    readonly property string label: window?.title ?? ""

    // Every window is the same height and as wide as its own shape, so a row
    // of them reads as the windows themselves.
    readonly property real wide: Math.max(4, Math.round(tall * (window?.aspect ?? 1.6)))
    // A tall, narrow window would otherwise leave no room for its title.
    readonly property real frameWidth: Math.max(wide, tall)

    width: frameWidth + 2 * Theme.cardPad
    height: 2 * Theme.cardPad + tall + Theme.titleTop + caption.implicitHeight

    // The window as Quickshell knows it, which is what gets captured.
    // Hyprland's own IPC writes an address with 0x in front, and Quickshell
    // without.
    readonly property var toplevel: {
        const address = (window?.id ?? "").replace(/^0x/, "");
        const toplevels = Hyprland.toplevels.values;

        for (let i = 0; i < toplevels.length; i++) {
            if (toplevels[i].address === address)
                return toplevels[i];
        }

        return null;
    }

    // How far its picture has come in: it fades up when the first frame
    // lands, rather than popping in over the icon standing in for it.
    property real arrived: capture.hasContent ? 1 : 0

    Behavior on arrived {
        enabled: card.animate

        NumberAnimation {
            duration: Theme.duration
            easing.type: Easing.BezierSpline
            easing.bezierCurve: Theme.curve
        }
    }

    // Whether this window has started being captured. Not until the switcher
    // is on screen, so that setting a capture up never holds up the
    // switcher's first frame; and from then on for as long as the card lasts,
    // so that the next switch opens on what the window last looked like.
    property bool started: false

    Component.onCompleted: started = capturing

    onCapturingChanged: {
        if (!capturing)
            return;

        // Only the application being switched to is kept up to date. The
        // others are taken again each time the switcher appears, so that
        // switching to one shows it as it is now. A new capture takes its
        // first picture by itself.
        if (started && !capture.live && capture.hasContent)
            capture.captureFrame();

        started = true;
    }

    ClippingRectangle {
        x: Theme.cardPad
        y: Theme.cardPad
        width: card.frameWidth
        height: card.tall
        radius: Theme.cardRadius
        color: Theme.cardFill
        border.width: 1
        border.color: Theme.cardEdge
        contentUnderBorder: true

        ScreencopyView {
            id: capture

            anchors.centerIn: parent
            width: card.wide
            height: card.tall
            visible: card.previews
            captureSource: card.previews && card.started ? (card.toplevel?.wayland ?? null) : null
            live: card.capturing && card.current
            opacity: card.arrived
        }

        // Some windows take a while to come back, or never do, and without
        // previews there are no pictures at all: the application's own icon
        // says which window this is, where an empty frame says only that
        // something is missing.
        AppIcon {
            anchors.centerIn: parent
            width: Math.round(card.tall / 2.5)
            height: width
            names: card.icons
            opacity: card.previews ? Theme.standin * (1 - card.arrived) : 0.8
            visible: opacity > 0
        }
    }

    // What the window is called, behind the key that reaches it.
    Row {
        id: caption

        x: Theme.cardPad + 2
        y: Theme.cardPad + card.tall + Theme.titleTop
        width: card.frameWidth - 4
        spacing: 5

        Text {
            anchors.verticalCenter: parent.verticalCenter
            // The title takes what the key leaves, so the key keeps to the
            // end of the line however short the title is.
            width: caption.width - (number.visible ? number.width + caption.spacing : 0)
            text: card.label
            color: card.selected ? Theme.selectedText : Theme.text
            font.family: card.family
            font.pixelSize: 12
            elide: Text.ElideMiddle

            Behavior on color {
                enabled: card.animate

                ColorAnimation {
                    duration: Theme.cycle
                    easing.type: Easing.BezierSpline
                    easing.bezierCurve: Theme.curve
                }
            }
        }

        Keycap {
            id: number

            anchors.verticalCenter: parent.verticalCenter
            text: card.number
            family: card.mono
            visible: card.number > 0
        }
    }
}
