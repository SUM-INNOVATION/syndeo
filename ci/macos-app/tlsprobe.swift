// EXPERIMENT (#58), removed before review: how each of macOS's stacks treats
// one https host through Syndeo's proxy.
//
//   tlsprobe <webkit|session|nw> <host> <proxy host:port> <ca.pem>
//
// webkit: a WKWebView, its proxy set as Syndeo.app sets it; session: a
// URLSession through the same proxy; nw: a bare Network framework TLS
// connection through it. Each logs every trust question it is asked, what
// macOS's default evaluation and the pinned one say, and on failure the
// whole error. One RESULT line, then it exits.
import AppKit
import Network
import WebKit

func say(_ line: String) {
    print(line.replacingOccurrences(of: "\n", with: " "))
    fflush(stdout)
}

struct Arguments {
    let mode: String
    let host: String
    let proxyHost: String
    let proxyPort: UInt16
    let anchor: SecCertificate
}

func parse() -> Arguments {
    let arguments = CommandLine.arguments
    guard arguments.count == 5, let colon = arguments[3].lastIndex(of: ":"),
        let port = UInt16(arguments[3][arguments[3].index(after: colon)...]),
        let pem = try? String(contentsOfFile: arguments[4], encoding: .utf8),
        let der = Data(base64Encoded: pem.split(separator: "\n").filter { !$0.hasPrefix("-----") }.joined()),
        let anchor = SecCertificateCreateWithData(nil, der as CFData)
    else {
        say("usage: tlsprobe <webkit|session|nw> <host> <proxy host:port> <ca.pem>")
        exit(2)
    }
    return Arguments(
        mode: arguments[1], host: arguments[2], proxyHost: String(arguments[3][..<colon]), proxyPort: port,
        anchor: anchor)
}

let parsed = parse()
let mode = parsed.mode
let host = parsed.host
let proxyHost = parsed.proxyHost
let proxyPort = parsed.proxyPort
let anchor = parsed.anchor

func trustSummary(_ trust: SecTrust) -> String {
    var parts: [String] = []
    if let result = SecTrustCopyResult(trust) as? [String: Any] {
        parts.append("result=\(result)")
    }
    var policies: CFArray?
    if SecTrustCopyPolicies(trust, &policies) == errSecSuccess, let list = policies as? [SecPolicy] {
        for policy in list {
            parts.append("policy=\(SecPolicyCopyProperties(policy) as? [String: Any] ?? [:])")
        }
    }
    if let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate] {
        parts.append(
            "chain=" + chain.map { (SecCertificateCopySubjectSummary($0) as String?) ?? "?" }.joined(separator: " < "))
    }
    return parts.joined(separator: " ")
}

func describe(_ error: Error) -> String {
    let ns = error as NSError
    var parts = ["\(ns.domain) \(ns.code)"]
    for (key, value) in ns.userInfo.sorted(by: { $0.key < $1.key }) {
        if key == NSUnderlyingErrorKey, let underlying = value as? Error {
            parts.append("underlying=[\(describe(underlying))]")
        } else if CFGetTypeID(value as AnyObject) == SecTrustGetTypeID() {
            parts.append("\(key)=trust{\(trustSummary(value as! SecTrust))}")
        } else {
            parts.append("\(key)=\(String(describing: value).prefix(400))")
        }
    }
    return parts.joined(separator: " | ")
}

var asked = 0

/// macOS's own verdict on `trust`, then the pinned one Syndeo gives.
func pinned(_ trust: SecTrust, _ by: String) -> Bool {
    asked += 1
    var plain: CFError?
    let plainOK = SecTrustEvaluateWithError(trust, &plain)
    say("TRUST \(mode) \(host) asked-by=\(by) default=\(plainOK) \(plain.map { "\($0)" } ?? "") \(trustSummary(trust))")
    SecTrustSetAnchorCertificates(trust, [anchor] as CFArray)
    SecTrustSetAnchorCertificatesOnly(trust, true)
    var error: CFError?
    let ok = SecTrustEvaluateWithError(trust, &error)
    say("TRUST \(mode) \(host) pinned=\(ok) \(error.map { "\($0)" } ?? "")")
    return ok
}

func finish(_ ok: Bool, _ detail: String) -> Never {
    say("RESULT \(mode) \(host) \(ok ? "ok" : "FAIL") asked=\(asked) \(detail)")
    exit(ok ? 0 : 1)
}

let url = URL(string: "https://\(host)/")!
let endpoint = NWEndpoint.hostPort(host: .init(proxyHost), port: .init(rawValue: proxyPort)!)

final class Web: NSObject, WKNavigationDelegate {
    let window = NSWindow(
        contentRect: NSRect(x: 0, y: 0, width: 800, height: 600), styleMask: [.titled], backing: .buffered,
        defer: false)
    var view: WKWebView?

    func start() {
        window.makeKeyAndOrderFront(nil)
        let store = WKWebsiteDataStore.default()
        let config = nw_proxy_config_create_http_connect(
            nw_endpoint_create_host(proxyHost, String(proxyPort)), nil)
        store.setValue([config], forKey: "proxyConfigurations")
        store.removeData(ofTypes: WKWebsiteDataStore.allWebsiteDataTypes(), modifiedSince: .distantPast) {
            let configuration = WKWebViewConfiguration()
            configuration.websiteDataStore = store
            let view = WKWebView(frame: self.window.contentView!.bounds, configuration: configuration)
            view.navigationDelegate = self
            self.window.contentView = view
            self.view = view
            view.load(URLRequest(url: url))
        }
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        finish(true, "title=\(webView.title ?? "")")
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        finish(false, describe(error))
    }

    func webView(
        _ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error
    ) {
        finish(false, describe(error))
    }

    @objc(webView:didReceiveAuthenticationChallenge:completionHandler:)
    func webView(
        _ webView: WKWebView, didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping @MainActor @Sendable (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        let space = challenge.protectionSpace
        say("CHALLENGE webkit \(host) for=\(space.host):\(space.port) method=\(space.authenticationMethod)")
        guard space.authenticationMethod == NSURLAuthenticationMethodServerTrust, let trust = space.serverTrust
        else {
            completionHandler(.performDefaultHandling, nil)
            return
        }
        if pinned(trust, "webkit-delegate") {
            completionHandler(.useCredential, URLCredential(trust: trust))
        } else {
            completionHandler(.cancelAuthenticationChallenge, nil)
        }
    }
}

final class Session: NSObject, URLSessionDelegate, URLSessionTaskDelegate {
    func start() {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.proxyConfigurations = [ProxyConfiguration(httpCONNECTProxy: endpoint)]
        let session = URLSession(configuration: configuration, delegate: self, delegateQueue: .main)
        session.dataTask(with: url) { data, response, error in
            if let error {
                finish(false, describe(error))
            }
            let status = (response as? HTTPURLResponse)?.statusCode ?? 0
            finish(true, "status=\(status) bytes=\(data?.count ?? 0)")
        }.resume()
    }

    func urlSession(
        _ session: URLSession, didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        answer(challenge, "session-delegate", completionHandler)
    }

    func urlSession(
        _ session: URLSession, task: URLSessionTask, didReceive challenge: URLAuthenticationChallenge,
        completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        answer(challenge, "task-delegate", completionHandler)
    }

    func answer(
        _ challenge: URLAuthenticationChallenge, _ by: String,
        _ completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void
    ) {
        let space = challenge.protectionSpace
        say("CHALLENGE session \(host) for=\(space.host):\(space.port) method=\(space.authenticationMethod) by=\(by)")
        guard space.authenticationMethod == NSURLAuthenticationMethodServerTrust, let trust = space.serverTrust
        else {
            completionHandler(.performDefaultHandling, nil)
            return
        }
        if pinned(trust, by) {
            completionHandler(.useCredential, URLCredential(trust: trust))
        } else {
            completionHandler(.cancelAuthenticationChallenge, nil)
        }
    }
}

func bare() {
    let tls = NWProtocolTLS.Options()
    sec_protocol_options_add_tls_application_protocol(tls.securityProtocolOptions, "h2")
    sec_protocol_options_add_tls_application_protocol(tls.securityProtocolOptions, "http/1.1")
    sec_protocol_options_set_verify_block(
        tls.securityProtocolOptions,
        { _, trust, complete in
            complete(pinned(sec_trust_copy_ref(trust).takeRetainedValue(), "nw-verify-block"))
        }, .main)
    let parameters = NWParameters(tls: tls)
    let context = NWParameters.PrivacyContext(description: "tlsprobe")
    context.proxyConfigurations = [ProxyConfiguration(httpCONNECTProxy: endpoint)]
    parameters.setPrivacyContext(context)
    let connection = NWConnection(host: .init(host), port: 443, using: parameters)
    connection.stateUpdateHandler = { state in
        switch state {
        case .ready: finish(true, "ready")
        case .failed(let error): finish(false, "failed: \(error) [\(describe(error))]")
        case .waiting(let error): say("STATE nw \(host) waiting: \(error)")
        default: say("STATE nw \(host) \(state)")
        }
    }
    connection.start(queue: .main)
}

DispatchQueue.main.asyncAfter(deadline: .now() + 30) { finish(false, "no answer within 30 s") }
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let web = Web()
let session = Session()
DispatchQueue.main.async {
    switch mode {
    case "webkit": web.start()
    case "session": session.start()
    case "nw": bare()
    default:
        say("unknown mode \(mode)")
        exit(2)
    }
}
app.run()
