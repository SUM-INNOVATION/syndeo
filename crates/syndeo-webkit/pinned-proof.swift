import Foundation
import WebKit
import Network
import Security

// The whole design in one file: WebKit renders, every byte goes through our
// proxy, and the proxy's authority is trusted *here only* — pinned in this
// process for this one proxy, never added to the system trust store.
let caPath = CommandLine.arguments[1]
let url = CommandLine.arguments[2]

func loadAnchor(_ path: String) -> SecCertificate? {
    guard let pem = try? String(contentsOfFile: path, encoding: .utf8) else { return nil }
    let b64 = pem.components(separatedBy: .newlines)
        .filter { !$0.hasPrefix("-----") && !$0.isEmpty }.joined()
    guard let der = Data(base64Encoded: b64) else { return nil }
    return SecCertificateCreateWithData(nil, der as CFData)
}
guard let anchor = loadAnchor(caPath) else { print("  could not read the authority"); exit(1) }

final class Pinner: NSObject, WKNavigationDelegate {
    let anchor: SecCertificate
    init(anchor: SecCertificate) { self.anchor = anchor }

    func webView(_ w: WKWebView, didReceive challenge: URLAuthenticationChallenge,
                 completionHandler done: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust else { return done(.performDefaultHandling, nil) }
        // Our authority is the only extra anchor, and the hostname still has to
        // match: this trusts one issuer, not everything.
        SecTrustSetAnchorCertificates(trust, [anchor] as CFArray)
        SecTrustSetAnchorCertificatesOnly(trust, true)
        var err: CFError?
        if SecTrustEvaluateWithError(trust, &err) {
            done(.useCredential, URLCredential(trust: trust))
        } else {
            print("  pinned evaluation refused it: \(err.map { String(describing: $0) } ?? "?")")
            done(.cancelAuthenticationChallenge, nil)
        }
    }
    func webView(_ w: WKWebView, didFinish n: WKNavigation!) { print("  didFinish; title=\(w.title ?? "<empty>")") }
    func webView(_ w: WKWebView, didFailProvisionalNavigation n: WKNavigation!, withError e: Error) {
        print("  didFailProvisional: \(e.localizedDescription)")
    }
}

let cfg = WKWebViewConfiguration()
let store = WKWebsiteDataStore.nonPersistent()
store.proxyConfigurations = [ProxyConfiguration(
    httpCONNECTProxy: .hostPort(host: "127.0.0.1", port: 8899), tlsOptions: nil)]
cfg.websiteDataStore = store
let pinner = Pinner(anchor: anchor)
let view = WKWebView(frame: .init(x: 0, y: 0, width: 1200, height: 800), configuration: cfg)
view.navigationDelegate = pinner
print("loading \(url) through the proxy, trusting only our authority")
view.load(URLRequest(url: URL(string: url)!))
RunLoop.main.run(until: Date().addingTimeInterval(20))
