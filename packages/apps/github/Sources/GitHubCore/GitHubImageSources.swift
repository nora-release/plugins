import Foundation

/// Where to fetch each picture a Markdown body names: the address GitHub put in the body it rendered.
/// An attachment (`github.com/user-attachments/assets/<id>`) becomes a signed link on
/// private-user-images.githubusercontent.com, and a picture from elsewhere goes through GitHub's
/// image proxy, camo.githubusercontent.com. Both are fetched without a sign-in or a redirect.
public enum GitHubImageSources {
    /// A picture of a rendered body that GitHub serves, and the address it stands for.
    public struct Rendered: Equatable, Sendable {
        public let src: URL
        public let original: String?
    }

    private static let tag = try! NSRegularExpression(pattern: "<img\\b[^>]*>", options: [.caseInsensitive])
    private static let attribute = try! NSRegularExpression(pattern: "\\b(src|data-canonical-src)\\s*=\\s*\"([^\"]*)\"", options: [.caseInsensitive])
    private static let attachment = try! NSRegularExpression(pattern: "^https://github\\.com/user-attachments/assets/([0-9a-fA-F-]{20,})$")

    /// Where to fetch the picture a Markdown body names, given the pictures of the body GitHub rendered;
    /// nil when GitHub doesn't serve it.
    public static func resolve(_ source: String, in rendered: [Rendered]) -> URL? {
        if let url = URL(string: source), url.scheme == "https", let host = url.host, GitHubImages.hosts.contains(host) { return url }
        if let id = attachmentID(source) { return rendered.first { $0.src.absoluteString.contains(id) }?.src }
        return rendered.first { $0.original == source }?.src
    }

    /// Each picture of a rendered body that GitHub serves, with the address it stands for.
    public static func rendered(_ html: String) -> [Rendered] {
        let whole = NSRange(html.startIndex..., in: html)
        return tag.matches(in: html, range: whole).compactMap { match in
            guard let range = Range(match.range, in: html) else { return nil }
            let element = String(html[range])
            var values: [String: String] = [:]
            for found in attribute.matches(in: element, range: NSRange(element.startIndex..., in: element)) {
                guard let name = Range(found.range(at: 1), in: element), let value = Range(found.range(at: 2), in: element) else { continue }
                values[element[name].lowercased()] = decodeEntities(String(element[value]))
            }
            guard let source = values["src"], let url = URL(string: source), url.scheme == "https",
                  let host = url.host, GitHubImages.hosts.contains(host) else { return nil }
            return Rendered(src: url, original: values["data-canonical-src"] ?? proxiedAddress(url))
        }
    }

    /// The address a camo link stands for: its last path segment is that address in hexadecimal.
    static func proxiedAddress(_ url: URL) -> String? {
        guard url.host == "camo.githubusercontent.com" else { return nil }
        let hex = url.lastPathComponent
        guard hex.count.isMultiple(of: 2), !hex.isEmpty else { return nil }
        var bytes: [UInt8] = []
        var index = hex.startIndex
        while index < hex.endIndex {
            let next = hex.index(index, offsetBy: 2)
            guard let byte = UInt8(hex[index ..< next], radix: 16) else { return nil }
            bytes.append(byte)
            index = next
        }
        return String(bytes: bytes, encoding: .utf8)
    }

    private static func attachmentID(_ source: String) -> String? {
        let range = NSRange(source.startIndex..., in: source)
        guard let match = attachment.firstMatch(in: source, range: range), let id = Range(match.range(at: 1), in: source) else { return nil }
        return String(source[id])
    }

    private static func decodeEntities(_ value: String) -> String {
        value.replacingOccurrences(of: "&amp;", with: "&").replacingOccurrences(of: "&quot;", with: "\"")
    }
}
