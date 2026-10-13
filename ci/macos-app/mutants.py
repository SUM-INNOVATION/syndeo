#!/usr/bin/env python3
"""Syndeo.app with one safeguard taken out, which ci/test-app.sh must catch.

  mutants.py names                 each mutant's name, one a line
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
- any-scheme-in-a-new-tab: a page asking for a new window gets a tab
  whatever the scheme. A page's links and scripts to other schemes in a new
  tab must then be seen opening one.
- no-editing-guard: the address field is written whenever a page reports
  its address, whoever is typing there. What someone types while a page is
  loading must then be seen overwritten when it arrives.
"""
import os
import sys

BROWSER = "crates/syndeo-app/src/browser_macos.rs"

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
            "            let allowed = !main_frame || address::allowed_in_main_frame(&scheme);\n"
            "            if !allowed {\n"
            "                self.ivars().refused.set(true);\n"
            "                tracing::info!(scheme = %scheme, \"refused a page that is not the web's own in a window\");\n"
            "            }\n"
            "            handler.call((if allowed { WKNavigationActionPolicy::Allow } else { WKNavigationActionPolicy::Cancel },));\n"
            "        }\n"
            "\n",
            "",
        ),
    ],
    "any-scheme-in-a-new-tab": [
        (
            BROWSER,
            "            if address::allowed_in_main_frame(&scheme_of(&request)) {\n",
            "            if true {\n",
        ),
    ],
    "no-editing-guard": [
        (
            BROWSER,
            "        if self.ivars().typing.get() {\n",
            "        if false {\n",
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
    if argv == ["names"]:
        print("\n".join(MUTANTS))
        return 0
    if len(argv) == 2 and argv[0] == "--check":
        return check(argv[1])
    if len(argv) == 3 and argv[0] == "apply":
        return apply(argv[1], argv[2])
    sys.exit("usage: mutants.py names | --check <repo> | apply <name> <repo>")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
