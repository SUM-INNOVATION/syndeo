// EXPERIMENT (#58), removed before review: what excluding a domain from
// WebKit's proxy configuration does.
//
//   directprobe load <proxy host:port> <ca.pem> <excluded,...|-> <url>
//       One load, with those domains excluded. A server-trust challenge for
//       a host matching an excluded domain (itself, or a subdomain) gets the
//       default handling, macOS's own trust; any other is pinned to ca.pem.
//   directprobe update <proxy host:port> <ca.pem> <url1> <excluded> <url2> <url3>
//       url1 through the proxy; then the store's configuration replaced with
//       one that excludes <excluded>; then url2 and url3 in the same web view.
//
// One line a load: LOAD <url> ok|FAIL ... with the title, how many resources
// the page loaded and how many of its elements failed to.
import AppKit
import Network
import WebKit

func say(_ line: String) {
    print(line.replacingOccurrences(of: "\n", with: " "))
    fflush(stdout)
}

struct Arguments {
    let mode: String
    let proxyHost: String
    let proxyPort: UInt16
    let anchor: SecCertificate
    let rest: [String]
}

func parse() -> Arguments {
    let a = CommandLine.arguments
    guard a.count >= 5, let colon = a[2].lastIndex(of: ":"),
        let port = UInt16(a[2][a[2].index(after: colon)...]),
        let pem = try? String(contentsOfFile: a[3], encoding: .utf8),
        let der = Data(base64Encoded: pem.split(separator: "\n").filter { !$0.hasPrefix("-----") }.joined()),
        let anchor = SecCertificateCreateWithData(nil, der as CFData)
    else {
        say("usage: directprobe <load|update> <proxy host:port> <ca.pem> ...")
        exit(2)
    }
    return Arguments(
        mode: a[1], proxyHost: String(a[2][..<colon]), proxyPort: port, anchor: anchor, rest: Array(a[4...]))
}

let args = parse()
var excluded: [String] = []

func config(excluding domains: [String]) -> nw_proxy_config_t {
    let config = nw_proxy_config_create_http_connect(
        nw_endpoint_create_host(args.proxyHost, String(args.proxyPort)), nil)
    for domain in domains {
        nw_proxy_config_add_excluded_domain(config, domain)
    }
    return config
}

func isExcluded(_ host: String) -> Bool {
    excluded.contains { host == $0 || host.hasSuffix("." + $0) }
}

/// The page's title, resources and failed elements, as JSON text.
let collect = """
    (() => JSON.stringify({
      title: document.title,
      resources: performance.getEntriesByType('resource').length,
      failed: window.__failed || 0,
      text: (document.body ? document.body.innerText : '').slice(0, 120)
    }))()
    """
let watchFailures = WKUserScript(
    source: "window.__failed = 0; window.addEventListener('error', e => { if (e.target && e.target !== window) window.__failed++; }, true);",
    injectionTime: .atDocumentStart, forMainFrameOnly: true)

final class Driver: NSObject, WKNavigationDelegate {
    let window = NSWindow(
        contentRect: NSRect(x: 0, y: 0, width: 1024, height: 768), styleMask: [.titled], backing: .buffered,
        defer: false)
    var view: WKWebView!
    var store: WKWebsiteDataStore!
    var queue: [() -> Void] = []
    var current = ""

    func start(_ steps: [() -> Void]) {
        window.makeKeyAndOrderFront(nil)
        store = WKWebsiteDataStore.default()
        store.setValue([config(excluding: excluded)], forKey: "proxyConfigurations")
        store.removeData(ofTypes: WKWebsiteDataStore.allWebsiteDataTypes(), modifiedSince: .distantPast) {
            let configuration = WKWebViewConfiguration()
            configuration.websiteDataStore = self.store
            configuration.userContentController.addUserScript(watchFailures)
            self.view = WKWebView(frame: self.window.contentView!.bounds, configuration: configuration)
            self.view.navigationDelegate = self
            self.window.contentView = self.view
            self.queue = steps
            self.next()
        }
    }

    func next() {
        guard !queue.isEmpty else { exit(0) }
        queue.removeFirst()()
    }

    func load(_ url: String) {
        current = url
        view.load(URLRequest(url: URL(string: url)!))
    }

    func done(_ ok: Bool, _ detail: String) {
        say("LOAD \(current) \(ok ? "ok" : "FAIL") excluded=\(excluded.joined(separator: ",")) \(detail)")
        DispatchQueue.main.async { self.next() }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        DispatchQueue.main.asyncAfter(deadline: .now() + 2) {
            webView.evaluateJavaScript(collect) { value, _ in
                self.done(true, (value as? String) ?? "{}")
            }
        }
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        let ns = error as NSError
        done(false, "\(ns.domain) \(ns.code) \(ns.localizedDescription)")
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        let ns = error as NSError
        done(false, "\(ns.domain) \(ns.code) \(ns.localizedDescription)")
    }

    #if compiler(>=6.0)
        @objc(webView:didReceiveAuthenticationChallenge:completionHandler:)
        func webView(
            _ webView: WKWebView, didReceive challenge: URLAuthenticationChallenge,
            completionHandler: @escaping @MainActor @Sendable (URLSession.AuthChallengeDisposition, URLCredential?)
                -> Void
        ) {
            let (disposition, credential) = decide(challenge)
            completionHandler(disposition, credential)
        }
    #else
        func webView(_ webView: WKWebView, respondTo challenge: URLAuthenticationChallenge) async
            -> (URLSession.AuthChallengeDisposition, URLCredential?)
        {
            decide(challenge)
        }
    #endif

    func decide(_ challenge: URLAuthenticationChallenge) -> (URLSession.AuthChallengeDisposition, URLCredential?) {
        let space = challenge.protectionSpace
        guard space.authenticationMethod == NSURLAuthenticationMethodServerTrust, let trust = space.serverTrust
        else { return (.performDefaultHandling, nil) }
        if isExcluded(space.host.lowercased()) {
            say("TRUST \(space.host) default")
            return (.performDefaultHandling, nil)
        }
        SecTrustSetAnchorCertificates(trust, [args.anchor] as CFArray)
        SecTrustSetAnchorCertificatesOnly(trust, true)
        var error: CFError?
        if SecTrustEvaluateWithError(trust, &error) {
            say("TRUST \(space.host) pinned ok")
            return (.useCredential, URLCredential(trust: trust))
        }
        say("TRUST \(space.host) pinned refused \(error.map { "\($0)" } ?? "")")
        return (.cancelAuthenticationChallenge, nil)
    }
}

let driver = Driver()
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
DispatchQueue.main.asyncAfter(deadline: .now() + 120) {
    say("TIMEOUT")
    exit(1)
}
DispatchQueue.main.async {
    switch args.mode {
    case "load" where args.rest.count == 2:
        excluded = args.rest[0] == "-" ? [] : args.rest[0].split(separator: ",").map(String.init)
        driver.start([{ driver.load(args.rest[1]) }])
    case "update" where args.rest.count == 4:
        driver.start([
            { driver.load(args.rest[0]) },
            {
                excluded = [args.rest[1]]
                driver.store.setValue([config(excluding: excluded)], forKey: "proxyConfigurations")
                say("UPDATED excluded=\(args.rest[1])")
                driver.load(args.rest[2])
            },
            { driver.load(args.rest[3]) },
        ])
    default:
        say("bad arguments")
        exit(2)
    }
}
app.run()
