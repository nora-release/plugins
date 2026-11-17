import Foundation

/// The arguments the agent gave a command, read strictly: a wrong type is an error, never a default.
struct GitHubCommandArguments {
    static let maximumTextBytes = 65536
    static let maximumPathBytes = 2048
    static let maximumNames = 100
    let values: GitHubValue

    func repository() throws -> String {
        let name = values["repository"].text
        guard GitHubAPI.isValidRepository(name) else { throw GitHubAPIError.invalidRepository }
        return name
    }

    func number() throws -> Int {
        guard let number = values["number"].int, number > 0 else { throw invalid("number must be a positive integer.") }
        return number
    }

    func page() throws -> Int {
        if values["page"].isNull { return 1 }
        guard let page = values["page"].int, page > 0 else { throw invalid("page must be a positive integer.") }
        return page
    }

    func state() throws -> GitHubItemState {
        if values["state"].isNull { return .open }
        guard let state = values["state"].string.flatMap(GitHubItemState.init(rawValue:)) else { throw invalid("state must be open, closed or all.") }
        return state
    }

    /// Text the agent may leave out. What it wrote is kept as written.
    func text(_ key: String, maximumBytes: Int = GitHubCommandArguments.maximumTextBytes) throws -> String? {
        if values[key].isNull { return nil }
        guard let text = values[key].string, text.utf8.count <= maximumBytes else {
            throw invalid("\(key) must be text of at most \(maximumBytes) bytes.")
        }
        return text
    }

    func requiredText(_ key: String, maximumBytes: Int = GitHubCommandArguments.maximumTextBytes) throws -> String {
        guard let text = try text(key, maximumBytes: maximumBytes), !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw invalid("\(key) is required.")
        }
        return text
    }

    func choice(_ key: String, from options: [String]) throws -> String? {
        guard let text = try text(key, maximumBytes: 64)?.lowercased() else { return nil }
        guard options.contains(text) else { throw invalid("\(key) must be one of: \(options.joined(separator: ", ")).") }
        return text
    }

    func flag(_ key: String) throws -> Bool? {
        if values[key].isNull { return nil }
        guard let flag = values[key].bool else { throw invalid("\(key) must be true or false.") }
        return flag
    }

    /// A list of label or account names, as the array GitHub takes.
    func names(_ key: String) throws -> GitHubValue? {
        if values[key].isNull { return nil }
        let names = values[key].array.compactMap(\.string)
        guard case .array(let rows) = values[key], names.count == rows.count, names.count <= Self.maximumNames,
              names.allSatisfy({ !$0.isEmpty && $0.utf8.count <= 256 }) else {
            throw invalid("\(key) must be a list of at most \(Self.maximumNames) names.")
        }
        return .array(names.map(GitHubValue.string))
    }

    /// A path of GitHub's REST API. The request itself refuses any host but api.github.com.
    func apiPath() throws -> String {
        let path = values["path"].text
        guard path.hasPrefix("/"), !path.hasPrefix("//"), path.utf8.count <= Self.maximumPathBytes, !path.contains("#"),
              !path.unicodeScalars.contains(where: CharacterSet.controlCharacters.contains),
              URL(string: "https://api.github.com" + path)?.host == "api.github.com" else {
            throw invalid("path must be a GitHub REST API path that starts with /, such as /repos/owner/name/releases.")
        }
        return path
    }

    private func invalid(_ reason: String) -> GitHubCommandError { .invalidArgument(reason) }
}
