var helperVersion = 2;
var requestKey = "request_json";
var responseKey = "response_json";
var snapshotKey = "snapshot_json";
var lastHandledRequest = "";
var pollIntervalMs = 50;
var helperApi = null;
var bridgeService = "org.cua.KWinHelper";
var bridgePath = "/org/cua/KWinHelper";
var bridgeInterface = "org.cua.KWinHelper";


function log(message) {
    console.log("CUA_KWIN_HELPER:", message);
}

function kwin() {
    return helperApi ? helperApi.kwin : null;
}

function workspaceApi() {
    return helperApi ? helperApi.workspace : null;
}

function scriptRoot() {
    return helperApi ? helperApi.scriptRoot : null;
}

function readHelperConfig(key, fallback) {
    if (helperApi && typeof helperApi.readConfig === "function") {
        return helperApi.readConfig(key, fallback);
    }
    if (kwin() && typeof kwin().readConfig === "function") {
        return kwin().readConfig(key, fallback);
    }
    if (typeof readConfig === "function") {
        return readConfig(key, fallback);
    }
    return fallback;
}

function publishBridge(method, value) {
    // Both supported entry points provide a normalized two-argument publisher:
    // the declarative wrapper uses its DBusCall object, while the legacy
    // wrapper adapts KWin's five-argument global callDBus. Imported JavaScript
    // modules do not reliably see the legacy global directly; using the API
    // object also prevents a stale QML instance from accidentally calling a
    // different KWin binding.
    var publisher = helperApi && helperApi.callDBus;
    if (typeof publisher !== "function") {
        log("callDBus is unavailable; helper publication remains disabled");
        return false;
    }
    try {
        publisher(method, String(value || ""));
        log("published " + method);
        return true;
    } catch (error) {
        log("callDBus publication failed for " + method + ": " + String(error));
        return false;
    }
}

function copyWindowList(list) {
    if (list === null || list === undefined) {
        return null;
    }
    if (typeof list === "function") {
        list = list();
    }
    if (list === null || list === undefined) {
        return null;
    }
    if (typeof list.length !== "number" || !isFinite(Number(list.length))) {
        return null;
    }
    var windows = [];
    for (var i = 0; i < Number(list.length); i++) {
        windows.push(list[i]);
    }
    return windows;
}

function workspaceWindows() {
    var workspace = workspaceApi();
    if (!workspace) {
        log("Workspace is unavailable; refusing to publish a snapshot");
        return null;
    }
    if (typeof workspace.windowList === "function") {
        try {
            return copyWindowList(workspace.windowList());
        } catch (error) {
            log("Workspace.windowList() failed: " + String(error));
            return null;
        }
    }
    // The declarative QML wrapper exposes windowList as a QQmlListProperty.
    var windows = copyWindowList(workspace.windowList);
    if (windows !== null) {
        return windows;
    }
    windows = copyWindowList(workspace.windows);
    if (windows !== null) {
        return windows;
    }
    log("Workspace.windowList() is unavailable; refusing to publish a snapshot");
    return null;
}

function connectSignal(signal, handler) {
    if (signal && typeof signal.connect === "function") {
        signal.connect(handler);
    }
}

// KWin 6 exposes Window.internalId as a QUuid. The public cua window contract
// is u64, so publish a deterministic 53-bit token that remains exactly
// representable in JavaScript and Rust. Collisions are rejected per snapshot.
function stableWindowId(internalId) {
    var text = String(internalId || "");
    var first = 2166136261;
    var second = 2166136261 ^ 0x9e3779b9;
    for (var i = 0; i < text.length; i++) {
        var code = text.charCodeAt(i);
        first = Math.imul(first ^ code, 16777619) >>> 0;
        second = Math.imul(second ^ code, 2246822519) >>> 0;
    }
    var token = (first & 0x1fffff) * 4294967296 + second;
    return token || 1;
}

function helperWindowRecord(client, index) {
    var frame = client.frameGeometry;
    var internalId = String(client.internalId || "");
    if (!internalId || !frame) {
        log("window is missing KWin internalId/frameGeometry; refusing to publish snapshot");
        return null;
    }
    var stacking = Number(client.stackingOrder);
    return {
        window_id: stableWindowId(internalId),
        internal_id: internalId,
        pid: Number(client.pid),
        title: String(client.caption || ""),
        app_id: String(client.desktopFileName || ""),
        resource_class: String(client.resourceClass || ""),
        active: !!client.active,
        minimized: !!client.minimized,
        visible: !client.hidden,
        x: Number(frame.x),
        y: Number(frame.y),
        width: Number(frame.width),
        height: Number(frame.height),
        stacking: isFinite(stacking) ? stacking : index
    };
}

function helperWindows() {
    var clients = workspaceWindows();
    if (clients === null) {
        return null;
    }
    var windows = [];
    var ids = {};
    for (var i = 0; i < clients.length; i++) {
        var client = clients[i];
        var pid = client ? Number(client.pid) : 0;
        if (!client || client.deleted || client.hidden || !client.normalWindow || !isFinite(pid) || pid <= 0) {
            continue;
        }
        var record = helperWindowRecord(client, i);
        if (record === null) {
            return null;
        }
        var idKey = String(record.window_id);
        if (ids[idKey]) {
            log("stable window-id collision; refusing to publish snapshot for " + idKey);
            return null;
        }
        ids[idKey] = true;
        windows.push(record);
    }
    return windows;
}

function publishCapabilities() {
    var payload = JSON.stringify({
        version: helperVersion,
        compositor: "kwin_wayland",
        can_snapshot: true,
        can_activate_exact_window: true,
        can_restore_focus: true
    });
    publishBridge("PublishCapabilities", payload);
}

function publishSnapshot() {
    var windows = helperWindows();
    if (windows === null) {
        return false;
    }
    var payload = JSON.stringify({
        version: helperVersion,
        windows: windows
    });
    log("publishing snapshot with " + windows.length + " windows");
    publishBridge("PublishSnapshot", payload);
    return true;
}

function windowForId(windowId) {
    var clients = workspaceWindows();
    if (clients === null) {
        return null;
    }
    for (var i = 0; i < clients.length; i++) {
        var client = clients[i];
        if (client && !client.deleted && stableWindowId(String(client.internalId || "")) === Number(windowId)) {
            return client;
        }
    }
    return null;
}

function activeWindowId() {
    var workspace = workspaceApi();
    return workspace.activeWindow
        ? stableWindowId(String(workspace.activeWindow.internalId || ""))
        : null;
}

function windowPid(windowId) {
    if (windowId === null || windowId === undefined) {
        return null;
    }
    var client = windowForId(windowId);
    return client ? Number(client.pid) : null;
}

function publishResponse(nonce, ok, previousWindowIdValue, activeWindowIdValue, error) {
    var payload = JSON.stringify({
        version: helperVersion,
        nonce: String(nonce || ""),
        ok: !!ok,
        previous_window_id: previousWindowIdValue,
        previous_pid: windowPid(previousWindowIdValue),
        active_window_id: activeWindowIdValue,
        active_pid: windowPid(activeWindowIdValue),
        error: String(error || "")
    });
    publishBridge("PublishResponse", payload);
    publishSnapshot();
}

function handleActivateRequest(request) {
    var workspace = workspaceApi();
    var client = windowForId(request.window_id);
    var previous = workspace.activeWindow;
    var previousWindowIdValue = previous
        ? stableWindowId(String(previous.internalId || ""))
        : null;
    if (!client) {
        publishResponse(request.nonce, false, previousWindowIdValue, previousWindowIdValue, "unknown window_id");
        return;
    }
    if (Number(request.pid) !== Number(client.pid)) {
        publishResponse(
            request.nonce,
            false,
            previousWindowIdValue,
            previousWindowIdValue,
            "exact pid/window_id mismatch"
        );
        return;
    }
    if (client.minimized) {
        client.minimized = false;
    }
    workspace.activeWindow = client;
    var active = workspace.activeWindow;
    var activeId = active ? stableWindowId(String(active.internalId || "")) : null;
    var activePid = active ? Number(active.pid) : null;
    publishResponse(
        request.nonce,
        activeId === Number(request.window_id) && activePid === Number(request.pid),
        previousWindowIdValue,
        activeId,
        ""
    );
}

function pollRequests() {
    var raw = readHelperConfig(requestKey, "");
    log("poll request raw length=" + String(raw || "").length);
    if (!raw || raw === lastHandledRequest) {
        helperSetTimeout(pollRequests, pollIntervalMs);
        return;
    }
    lastHandledRequest = raw;

    try {
        var request = JSON.parse(raw);
        var nonce = String(request.nonce || "");
        if (!nonce) {
            publishResponse("", false, activeWindowId(), activeWindowId(), "missing request nonce");
        } else if (Number(request.version) !== helperVersion) {
            publishResponse(nonce, false, activeWindowId(), activeWindowId(), "unsupported version");
        } else if (request.action !== "activate") {
            publishResponse(nonce, false, activeWindowId(), activeWindowId(), "unsupported action");
        } else if (!isFinite(Number(request.pid)) || Number(request.pid) <= 0) {
            publishResponse(nonce, false, activeWindowId(), activeWindowId(), "missing request pid");
        } else {
            handleActivateRequest({
                nonce: nonce,
                pid: Number(request.pid),
                window_id: request.window_id
            });
    }
    } catch (error) {
        publishResponse("", false, activeWindowId(), activeWindowId(), String(error));
    }
    helperSetTimeout(pollRequests, pollIntervalMs);
}

function retryPublication() {
    publishCapabilities();
    publishSnapshot();
    helperSetTimeout(retryPublication, 1000);
}

function bindSnapshotSignals(client) {
    if (!client) {
        return;
    }
    connectSignal(client.bufferGeometryChanged, publishSnapshot);
    connectSignal(client.minimizedChanged, publishSnapshot);
    connectSignal(client.desktopsChanged, publishSnapshot);
    connectSignal(client.fullScreenChanged, publishSnapshot);
}

function bindWorkspaceSignals() {
    var workspace = workspaceApi();
    connectSignal(workspace.windowAdded, function(client) {
        bindSnapshotSignals(client);
        publishSnapshot();
    });
    connectSignal(workspace.windowRemoved, publishSnapshot);
    connectSignal(workspace.windowActivated, publishSnapshot);
    connectSignal(workspace.currentDesktopChanged, publishSnapshot);
    connectSignal(workspace.currentActivityChanged, publishSnapshot);
    connectSignal(workspace.virtualScreenGeometryChanged, publishSnapshot);
    connectSignal(workspace.screensChanged, publishSnapshot);
}

function helperSetTimeout(func, timeout) {
    if (typeof setTimeout === "function") {
        setTimeout(func, timeout);
        return true;
    }
    // Legacy KWin JavaScript scripts do not expose Qt or a QML object tree.
    // Requests are delivered by options.configChanged in that environment;
    // refusing to manufacture a timer keeps the script alive instead of
    // throwing on the first poll.
    if (typeof Qt === "undefined" || !scriptRoot()) {
        return false;
    }
    var timer = Qt.createQmlObject("import QtQuick 2.0; Timer {}", scriptRoot());
    timer.interval = timeout;
    timer.repeat = false;
    timer.triggered.connect(function() {
        timer.destroy();
        func();
    });
    timer.start();
    return true;
}

function bindConfigSignals() {
    if (typeof options !== "undefined" && options.configChanged
            && typeof options.configChanged.connect === "function") {
        options.configChanged.connect(function() {
            log("options.configChanged fired");
            try {
                pollRequests();
            } catch (error) {
                log("request poll failed: " + String(error));
            }
        });
        log("bound options.configChanged request signal");
    }
}

function main(api) {
    helperApi = api;
    if (!workspaceApi()) {
        log("Workspace API is unavailable; helper will not start");
        return;
    }

    log("started; windowList=" + (typeof workspaceApi().windowList === "function")
        + ", readConfig=" + (typeof helperApi.readConfig === "function")
        + ", callDBus=" + (typeof helperApi.callDBus === "function"));

    publishCapabilities();
    publishSnapshot();
    bindConfigSignals();
    bindWorkspaceSignals();

    var clients = workspaceWindows() || [];
    for (var i = 0; i < clients.length; i++) {
        bindSnapshotSignals(clients[i]);
    }

    // Legacy KWin JS has no timer API. Process a request already present in
    // kwinrc synchronously at startup; subsequent requests arrive through the
    // options.configChanged signal above.
    pollRequests();
}

// Plasma 6's legacy JavaScript KWin script API is the stable live entry
// point. It provides the global objects/functions used by upstream scripts.
if (typeof workspace !== "undefined") {
    main({
        workspace: workspace,
        kwin: typeof KWin !== "undefined" ? KWin : null,
        readConfig: readConfig,
        callDBus: function(method, value) {
            callDBus(
                bridgeService,
                bridgePath,
                bridgeInterface,
                method,
                String(value || "")
            );
        },
        scriptRoot: null
    });
}
