// How long WebKit takes to show a page, directly or through a proxy. Used by
// ci/test-pageload.sh; compiled once per run with swiftc.
//
//   pageload <url> <runs>                          WebKit by itself
//   pageload <url> <runs> <host:port> <ca.pem>     through the HTTP CONNECT
//                                                  proxy at host:port, accepting
//                                                  only leaves that chain to
//                                                  ca.pem, as Syndeo.app's
//                                                  windows do
//
// Each run is a new web view with a new, empty, non-persistent data store,
// so nothing WebKit kept from another run answers for it: the url is loaded
// once (cold) and then again in the same web view (warm). One JSON object a
// line per load, on stdout:
//
//   {"run":1,"visit":"cold","ok":true,"fcp":812,"dcl":901,"load":1520,
//    "wall":1604,"resources":131,"protocols":{"h2":131,"http/1.1":1}}
//
// fcp, dcl and load are the page's own Paint and Navigation Timing (first
// contentful paint, DOMContentLoaded's end, load's end), in milliseconds from
// the navigation's start; wall is from asking for the page to WebKit saying it
// finished. protocols counts nextHopProtocol over the navigation and every
// resource the page loaded. A load that fails or takes over 60 s is
// {"ok":false,"error":...}.
import AppKit
import Network
import WebKit

struct Proxy {
    let host: String
    let port: UInt16
    let anchor: SecCertificate
}

func usage(_ message: String) -> Never {
    FileHandle.standardError.write("pageload: \(message)\n".data(using: .utf8)!)
    exit(2)
}

func parse() -> (URL, Int, Proxy?) {
    let arguments = CommandLine.arguments
    guard arguments.count == 3 || arguments.count == 5, let url = URL(string: arguments[1]),
        let runs = Int(arguments[2]), runs > 0
    else { usage("usage: pageload <url> <runs> [<host:port> <ca.pem>]") }
    guard arguments.count == 5 else { return (url, runs, nil) }
    let parts = arguments[3].split(separator: ":")
    guard parts.count == 2, let port = UInt16(parts[1]),
        let pem = try? String(contentsOfFile: arguments[4], encoding: .utf8)
    else { usage("unusable proxy or certificate") }
    let base64 = pem.split(separator: "\n").filter { !$0.hasPrefix("-----") }.joined()
    guard let der = Data(base64Encoded: base64),
        let anchor = SecCertificateCreateWithData(nil, der as CFData)
    else { usage("the certificate does not parse") }
    return (url, runs, Proxy(host: String(parts[0]), port: port, anchor: anchor))
}

let (url, runs, proxy) = parse()

// Paint, Navigation and Resource Timing, as the page itself has them.
let collect = """
    (() => {
      const nav = performance.getEntriesByType('navigation')[0];
      const fcp = performance.getEntriesByName('first-contentful-paint')[0];
      const protocols = {};
      for (const e of [nav, ...performance.getEntriesByType('resource')]) {
        if (!e) continue;
        const p = e.nextHopProtocol || 'unknown';
        protocols[p] = (protocols[p] || 0) + 1;
      }
      return JSON.stringify({
        fcp: fcp ? Math.round(fcp.startTime) : null,
        dcl: nav ? Math.round(nav.domContentLoadedEventEnd) : null,
        load: nav ? Math.round(nav.loadEventEnd) : null,
        resources: performance.getEntriesByType('resource').length,
        protocols
      });
    })()
    """

final class Driver: NSObject, WKNavigationDelegate {
    let window = NSWindow(
        contentRect: NSRect(x: 0, y: 0, width: 1280, height: 800),
        styleMask: [.titled], backing: .buffered, defer: false)
    var webView: WKWebView?
    var run = 0
    var visit = "cold"
    var asked = Date()
    var timeout: Timer?

    func start() {
        window.title = "pageload"
        window.center()
        window.makeKeyAndOrderFront(nil)
        next()
    }

    func next() {
        if visit == "cold", webView != nil {
            visit = "warm"
        } else {
            run += 1
            visit = "cold"
            guard run <= runs else { exit(0) }
            let configuration = WKWebViewConfiguration()
            let store = WKWebsiteDataStore.nonPersistent()
            if let proxy {
                let endpoint = NWEndpoint.hostPort(
                    host: NWEndpoint.Host(proxy.host), port: NWEndpoint.Port(rawValue: proxy.port)!)
                store.proxyConfigurations = [ProxyConfiguration(httpCONNECTProxy: endpoint, tlsOptions: nil)]
            }
            configuration.websiteDataStore = store
            let view = WKWebView(frame: window.contentView!.bounds, configuration: configuration)
            view.navigationDelegate = self
            window.contentView = view
            webView = view
        }
        asked = Date()
        timeout?.invalidate()
        timeout = Timer.scheduledTimer(withTimeInterval: 60, repeats: false) { [weak self] _ in
            self?.report(["ok": false, "error": "no load within 60 s"])
        }
        webView!.load(URLRequest(url: url))
    }

    func report(_ fields: [String: Any]) {
        timeout?.invalidate()
        var line: [String: Any] = ["run": run, "visit": visit]
        line.merge(fields) { _, new in new }
        let data = try! JSONSerialization.data(withJSONObject: line, options: [.sortedKeys])
        print(String(data: data, encoding: .utf8)!)
        fflush(stdout)
        DispatchQueue.main.async { self.next() }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        let wall = Int(Date().timeIntervalSince(asked) * 1000)
        // A moment for paint timing to be recorded after load.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            webView.evaluateJavaScript(collect) { value, error in
                guard let text = value as? String, let data = text.data(using: .utf8),
                    var fields = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
                else {
                    self.report(["ok": false, "error": "timing: \(error?.localizedDescription ?? "none")"])
                    return
                }
                fields["ok"] = true
                fields["wall"] = wall
                self.report(fields)
            }
        }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        report(["ok": false, "error": error.localizedDescription])
    }

    func webView(
        _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error
    ) {
        report(["ok": false, "error": error.localizedDescription])
    }

    // Through the proxy, a server's certificate is accepted only when it
    // chains to the proxy's own authority, which no trust settings hold. Bound
    // to WebKit's selector by name, so no compiler's reading of the
    // completion handler's attributes can leave WebKit without it: without
    // it WebKit rejects every challenge. Each challenge is logged to stderr.
    @objc(webView:didReceiveAuthenticationChallenge:completionHandler:)
    func webView(
        _ webView: WKWebView, didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping @MainActor @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        let space = challenge.protectionSpace
        guard space.authenticationMethod == NSURLAuthenticationMethodServerTrust,
            let proxy, let trust = space.serverTrust
        else {
            note("challenge \(space.host) \(space.authenticationMethod): default handling")
            completionHandler(.performDefaultHandling, nil)
            return
        }
        SecTrustSetAnchorCertificates(trust, [proxy.anchor] as CFArray)
        SecTrustSetAnchorCertificatesOnly(trust, true)
        var error: CFError?
        if SecTrustEvaluateWithError(trust, &error) {
            completionHandler(.useCredential, URLCredential(trust: trust))
        } else {
            note("challenge \(space.host): refused: \(error.map { String(describing: $0) } ?? "no reason")")
            completionHandler(.cancelAuthenticationChallenge, nil)
        }
    }
}

func note(_ line: String) {
    FileHandle.standardError.write("pageload: \(line)\n".data(using: .utf8)!)
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let driver = Driver()
DispatchQueue.main.async { driver.start() }
app.run()
