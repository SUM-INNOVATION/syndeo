#!/usr/bin/env python3
"""Syndeo.app with one safeguard taken out, which ci/test-app.sh must catch.

  mutants.py names [app|agents]    each mutant's name, one a line: all of
                                   them, or the application's or the
                                   agents' (agent-*) alone
  mutants.py --check <repo>        each mutant's text is in <repo>'s source
                                   exactly once, so none has quietly stopped
                                   applying
  mutants.py apply <name> <repo>   make that mutant in <repo>'s source

CI builds each mutant into a Syndeo.app of its own and runs
`ci/test-app.sh mutant <name> <Syndeo.app>`, which passes only when the check
meant to catch that mutant fails, and only that way. Then it puts the source
back.

- no-pinning-delegate: the window's controller never becomes its web view's
  navigation delegate, and says it did. Nothing then answers TLS challenges
  with Syndeo's authority as the anchor, so https through the proxy, whose
  authority no trust settings hold, must fail.
- no-navigation-policy: the handler that keeps a window's main frame to the
  web's own pages and Syndeo's is gone. A page's links and scripts to other
  schemes must then be seen leaving the page.
- any-scheme-in-a-new-tab: a page asking for a new window, or a link
  followed with ⌘ or the middle button, gets a tab whatever the scheme. A
  page's links and scripts to other schemes in a new tab must then be seen
  opening one.
- asked-any-scheme: an address another application asks Syndeo to open is
  opened whatever its scheme, as long as it has a host. An ftp address and a
  file asked for that way must then be seen not refused with the words
  typing them gets.
- no-editing-guard: the address field is written whenever a page reports
  its address, whoever is typing there. What someone types while a page is
  loading must then be seen overwritten when it arrives.
- direct-for-every-name: the bar's rule calls every name other than
  localhost Direct.
  The bar must then be seen saying "Direct — not cached by Syndeo" for a
  page that came through the proxy by a name.
- zoned-links-followed: a page's link to an IPv6 address with a zone is not
  refused, and a failure to open one is not described as a refusal. The
  link must then be seen not refused with the words typing it gets.
- reload-from-cache: Reload reloads as WebKit's plain `reload` does, which
  let Syndeo's proxy answer from its own fresh copy without asking the
  origin. The reload must then be seen not consulting the origin: neither
  the proxy's revalidations nor its misses rise.
- tabs-kept-after-clear: Clear Browsing Data leaves the tabs closed, to
  open again, and the session kept from the last quit. Both must then be
  seen still there after a clear: History > Reopen Last Closed Tab still
  offered, and the session file still on disk.
- websocket-headers-stripped: in the proxy, a WebSocket handshake goes the
  ordinary way, which drops Upgrade and Connection with the other
  hop-by-hop headers, as it did before; that is what CI 39198465745
  measured. A WebSocket through Syndeo must then be seen failing: in the
  page, at the origin, which is sent no Upgrade, and in the proxy's count,
  which the handshake then moves.
- agent-approval-skipped: an agent's request for leave is granted for the
  session without the person being asked. The prompts must then be seen
  missing, and JavaScript denied must be seen running.
- agent-grant-unenforced: whatever an agent was allowed, every request it
  makes once allowed at all is done, whatever scope it needs. JavaScript without the script grant, and a tab opened by
  an agent allowed only to read, must then be seen.
- agent-peer-unchecked: the socket's peer is not checked to be this user. A
  connection from root must then be seen not refused, and asked about.
- agent-unproxied-tab: an agent's tab browses in a web view of its own
  making, with WebKit's default configuration and no delegate, rather than
  its window's, which is routed through the proxy and pinned to Syndeo's
  authority. Its https page must then be seen not opening, and not fetched
  by Syndeo's proxy, since nothing answers the proxy's certificate with
  that authority. (Its http page may still come through the proxy, which
  CI 39690228430 saw: the proxy is set on WebKit's data store.)
- agent-tabs-in-session: the session kept at quit holds an agent's tabs as
  well as the person's. The agent's tab must then be seen in the session
  file, and opened again at the next launch.
- agent-tabs-recently-closed: a tab an agent closes goes in History >
  Recently Closed like the person's. It must then be seen offered there, and
  Reopen Last Closed Tab offered for it.

The agent-* mutants run in the macos-agents job, beside its gate; the others
in macos-app's.
"""
import os
import sys

BROWSER = "crates/syndeo-app/src/browser_macos.rs"
ADDRESS = "crates/syndeo-app/src/address.rs"
SETTINGS = "crates/syndeo-app/src/settings_macos.rs"
PROXY = "crates/syndeo-proxy/src/main.rs"
CONTROL = "crates/syndeo-app/src/control_macos.rs"
APP = "crates/syndeo-app/src/app_macos.rs"

MUTANTS = {
    "no-pinning-delegate": [
        (
            BROWSER,
            "            web.view.setNavigationDelegate(Some(delegate));\n",
            "            let _ = delegate;\n",
        ),
        (
            BROWSER,
            "        unsafe { web.view.navigationDelegate() }.is_some_and(|set| {\n",
            "        true || unsafe { web.view.navigationDelegate() }.is_some_and(|set| {\n",
        ),
    ],
    "no-navigation-policy": [
        (
            BROWSER,
            "        #[unsafe(method(webView:decidePolicyForNavigationAction:decisionHandler:))]\n"
            "        fn decide_policy(\n"
            "            &self,\n"
            "            _webview: &WKWebView,\n"
            "            action: &WKNavigationAction,\n"
            "            handler: &block2::DynBlock<dyn Fn(WKNavigationActionPolicy)>,\n"
            "        ) {\n"
            "            let main_frame = unsafe { action.targetFrame() }.is_some_and(|frame| unsafe { frame.isMainFrame() });\n"
            "            let request = unsafe { action.request() };\n"
            "            let scheme = scheme_of(&request);\n"
            "            let target = address_of(&request);\n"
            "            // Refused wherever it would load, as typing it is.\n"
            "            let zoned = address::has_zone(&target);\n"
            "            // A link followed with ⌘ or the middle button: elsewhere, as a\n"
            "            // new tab is made, or, for an address no tab may show, here,\n"
            "            // where it is refused as any other.\n"
            "            if let Some(place) = Place::of(tabs::opening(click_of(action))) {\n"
            "                if !zoned && self.open_beside(request.clone(), place) {\n"
            "                    self.ivars().refused.set(main_frame);\n"
            "                    handler.call((WKNavigationActionPolicy::Cancel,));\n"
            "                    return;\n"
            "                }\n"
            "            }\n"
            "            let allowed = (!main_frame || address::allowed_in_main_frame(&scheme)) && !zoned;\n"
            "            if !allowed && main_frame {\n"
            "                self.ivars().refused.set(true);\n"
            "                tracing::info!(scheme = %scheme, zoned, \"refused a page in a window\");\n"
            "            }\n"
            "            handler.call((if allowed { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));\n"
            "            if zoned && main_frame {\n"
            "                self.show_page(&target, address::ZONED);\n"
            "            }\n"
            "        }\n"
            "\n",
            "",
        ),
    ],
    "any-scheme-in-a-new-tab": [
        (
            BROWSER,
            "        if address::allowed_in_main_frame(&scheme_of(&request)) {\n",
            "        if true {\n",
        ),
    ],
    "asked-any-scheme": [
        (
            ADDRESS,
            "    if scheme != \"http\" && scheme != \"https\" {\n",
            "    if false {\n",
        ),
    ],
    "no-editing-guard": [
        (
            BROWSER,
            "        if self.ivars().typing.get() {\n",
            "        if false {\n",
        ),
    ],
    "direct-for-every-name": [
        (
            ADDRESS,
            "        Host::Domain(_) => Route::Syndeo,\n",
            "        Host::Domain(_) => Route::Direct,\n",
        ),
    ],
    "zoned-links-followed": [
        (
            BROWSER,
            "            let zoned = address::has_zone(&target);\n",
            "            let zoned = false;\n",
        ),
        (
            BROWSER,
            "        let why = if address::has_zone(&address) {\n",
            "        let why = if false {\n",
        ),
    ],
    "reload-from-cache": [
        (
            BROWSER,
            "                    unsafe { web.view.reloadFromOrigin() };\n",
            "                    unsafe { web.view.reload() };\n",
        ),
    ],
    "tabs-kept-after-clear": [
        (
            SETTINGS,
            "                (clearing.forget_tabs)();\n",
            "",
        ),
    ],
    "websocket-headers-stripped": [
        (
            PROXY,
            "        websocket::Upgrading::WebSocket { key } => {\n"
            "            return websocket::open(proxy, req, url, key).await\n"
            "        }\n",
            "        websocket::Upgrading::WebSocket { .. } => {}\n",
        ),
    ],
    "agent-approval-skipped": [
        (
            CONTROL,
            "        let response = alert.runModal();\n",
            "        let response = NSAlertThirdButtonReturn;\n",
        ),
    ],
    "agent-grant-unenforced": [
        (
            CONTROL,
            "                if let Some(scope) = command.needs() {\n"
            "                    if !granted.allows(scope) {\n",
            "                if let Some(scope) = command.needs() {\n"
            "                    if false && !granted.allows(scope) {\n",
        ),
    ],
    "agent-peer-unchecked": [
        (
            CONTROL,
            "    if peer_macos::uid_of(&stream).ok() != Some(own_uid) {\n",
            "    if false && own_uid == 0 {\n",
        ),
    ],
    "agent-unproxied-tab": [
        (
            CONTROL,
            "        let (window, controller) = (self.hooks.new_window)(Some(request.clone()));\n",
            "        let (window, controller) = (self.hooks.new_window)(None);\n",
        ),
        (
            CONTROL,
            "        // The tab browses in its window's own web view: routed through the\n"
            "        // proxy and pinned to its authority, as every window's is.\n"
            "        let Some(view) = controller.web_view() else {\n",
            "        let view = Some(unsafe {\n"
            "            WKWebView::initWithFrame_configuration(\n"
            "                WKWebView::alloc(self.mtm),\n"
            "                window.frame(),\n"
            "                &objc2_web_kit::WKWebViewConfiguration::new(self.mtm),\n"
            "            )\n"
            "        });\n"
            "        if let Some(plain) = &view {\n"
            "            window.setContentView(Some(plain));\n"
            "        }\n"
            "        let Some(view) = view else {\n",
        ),
        (
            CONTROL,
            "        tracing::info!(tab = id, \"an agent opened a tab\");\n",
            "        let _ = unsafe { view.loadRequest(&request) };\n"
            "        tracing::info!(tab = id, \"an agent opened a tab\");\n",
        ),
    ],
    "agent-tabs-in-session": [
        (
            APP,
            "                .filter(|tab| !control_macos::is_agents(tab))\n",
            "                .filter(|tab| tab.isVisible() || !tab.isVisible())\n",
        ),
    ],
    "agent-tabs-recently-closed": [
        (
            APP,
            "        if let Some(snapshot) = snapshot.filter(|_| !agents) {\n",
            "        if let Some(snapshot) = snapshot.filter(|_| agents || !agents) {\n",
        ),
    ],
}


def read(repo, path):
    with open(os.path.join(repo, path), encoding="utf-8") as f:
        return f.read()


def check(repo):
    wrong = 0
    for name, edits in MUTANTS.items():
        for path, old, _ in edits:
            found = read(repo, path).count(old)
            if found == 1:
                print(f"  ok    {name}: applies to {path}")
            else:
                print(f"  WRONG {name}: its text is in {path} {found} times, not once")
                wrong += 1
    print(f"\n  {len(MUTANTS)} mutants, {wrong} wrong")
    return 1 if wrong else 0


def apply(name, repo):
    if name not in MUTANTS:
        sys.exit(f"mutants.py: no mutant {name}")
    texts = {}
    for path, old, new in MUTANTS[name]:
        text = texts.get(path, read(repo, path))
        if text.count(old) != 1:
            sys.exit(f"mutants.py: {name} does not apply to {path}")
        texts[path] = text.replace(old, new)
    for path, text in texts.items():
        with open(os.path.join(repo, path), "w", encoding="utf-8") as f:
            f.write(text)
    print(f"mutants.py: made {name}")
    return 0


def main(argv):
    if argv[:1] == ["names"] and len(argv) <= 2:
        group = argv[1] if len(argv) == 2 else None
        if group not in (None, "app", "agents"):
            sys.exit("mutants.py: names takes app or agents")
        agents = {name for name in MUTANTS if name.startswith("agent-")}
        chosen = [
            name for name in MUTANTS
            if group is None or (name in agents) == (group == "agents")
        ]
        print("\n".join(chosen))
        return 0
    if len(argv) == 2 and argv[0] == "--check":
        return check(argv[1])
    if len(argv) == 3 and argv[0] == "apply":
        return apply(argv[1], argv[2])
    sys.exit("usage: mutants.py names [app|agents] | --check <repo> | apply <name> <repo>")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
