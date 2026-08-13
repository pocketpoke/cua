import QtQuick 2.15
import org.kde.kwin
import "../code/main.js" as Helper

Item {
    id: scriptRoot

    DBusCall {
        id: bridgeCall
        service: "org.cua.KWinHelper"
        path: "/org/cua/KWinHelper"
    }

    Component.onCompleted: {
        console.log("CUA_KWIN_HELPER: declarative entry point completed");
        Helper.main({
            "workspace": Workspace,
            "readConfig": function(key, fallback) {
                return KWin.readConfig(key, fallback);
            },
            "callDBus": function(method, value) {
                bridgeCall.method = method;
                bridgeCall.arguments = [String(value || "")];
                bridgeCall.call();
                console.log("CUA_KWIN_HELPER: published " + method);
            },
            "scriptRoot": scriptRoot
        });
    }
}
