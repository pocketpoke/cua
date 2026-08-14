import QtQuick 2.15
import org.kde.kwin
import "../code/main.js" as Helper

Item {
    id: scriptRoot

    DBusCall {
        id: capabilitiesCall
        service: "org.cua.KWinHelper"
        path: "/org/cua/KWinHelper"
        interface: "org.cua.KWinHelper"
    }

    DBusCall {
        id: snapshotCall
        service: "org.cua.KWinHelper"
        path: "/org/cua/KWinHelper"
        interface: "org.cua.KWinHelper"
    }

    DBusCall {
        id: responseCall
        service: "org.cua.KWinHelper"
        path: "/org/cua/KWinHelper"
        interface: "org.cua.KWinHelper"
    }

    Component.onCompleted: {
        console.log("CUA_KWIN_HELPER: declarative entry point completed");
        Helper.main({
            "workspace": Workspace,
            "readConfig": function(key, fallback) {
                return KWin.readConfig(key, fallback);
            },
            "callDBus": function(method, value) {
                var call = capabilitiesCall;
                if (method === "PublishSnapshot") {
                    call = snapshotCall;
                } else if (method === "PublishResponse") {
                    call = responseCall;
                }
                call.method = method;
                call.arguments = [String(value || "")];
                call.call();
                console.log("CUA_KWIN_HELPER: published " + method);
            },
            "scriptRoot": scriptRoot
        });
    }
}
