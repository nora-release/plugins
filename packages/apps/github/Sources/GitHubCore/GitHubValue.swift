import Foundation

public enum GitHubValue: Codable, Hashable, Sendable {
    case null, bool(Bool), number(Double), string(String), array([GitHubValue]), object([String: GitHubValue])

    public init(from decoder: any Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() { self = .null }
        else if let value = try? container.decode(Bool.self) { self = .bool(value) }
        else if let value = try? container.decode(Double.self) { self = .number(value) }
        else if let value = try? container.decode(String.self) { self = .string(value) }
        else if let value = try? container.decode([GitHubValue].self) { self = .array(value) }
        else { self = .object(try container.decode([String: GitHubValue].self)) }
    }

    public func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .null: try container.encodeNil()
        case .bool(let value): try container.encode(value)
        case .number(let value): try container.encode(value)
        case .string(let value): try container.encode(value)
        case .array(let value): try container.encode(value)
        case .object(let value): try container.encode(value)
        }
    }

    public subscript(_ key: String) -> GitHubValue { object[key] ?? .null }
    public subscript(_ index: Int) -> GitHubValue { array.indices.contains(index) ? array[index] : .null }
    public var string: String? { if case .string(let value) = self { value } else { nil } }
    public var text: String { string ?? "" }
    public var int: Int? {
        guard case .number(let value) = self, value.isFinite, value.rounded() == value, abs(value) < 9_007_199_254_740_992 else { return nil }
        return Int(value)
    }
    public var bool: Bool? { if case .bool(let value) = self { value } else { nil } }
    public var array: [GitHubValue] { if case .array(let value) = self { value } else { [] } }
    public var object: [String: GitHubValue] { if case .object(let value) = self { value } else { [:] } }
    public var isNull: Bool { self == .null }
    public var id: String { self["id"].int.map(String.init) ?? self["node_id"].string ?? self["sha"].string ?? "" }
    public var date: Date? {
        guard let string else { return nil }
        let formatter = ISO8601DateFormatter()
        formatter.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = formatter.date(from: string) { return date }
        formatter.formatOptions = [.withInternetDateTime]
        return formatter.date(from: string)
    }
    public func setting(_ key: String, _ value: GitHubValue) -> GitHubValue {
        var result = object
        result[key] = value
        return .object(result)
    }
    public func merging(_ value: GitHubValue) -> GitHubValue { .object(object.merging(value.object) { _, new in new }) }
}

public enum GitHubKind: String, CaseIterable, Hashable, Sendable { case issues, pullRequests }
public enum GitHubSection: String, CaseIterable, Sendable {
    case repository, comments, reviews, reviewComments, timeline, checks, statuses, workflows, commits, files
}
public struct GitHubSectionState: Sendable {
    public var items: [GitHubValue] = []
    public var data: GitHubValue = .null
    public var loading = false
    public var loaded = false
    public var error: String?
    public var nextPage: Int?
    public init() {}
}
public struct GitHubPage: Codable, Sendable {
    public var items: [GitHubValue]
    public var nextPage: Int?
    public var isTruncated: Bool
    public init(items: [GitHubValue], nextPage: Int? = nil, isTruncated: Bool = false) { self.items = items; self.nextPage = nextPage; self.isTruncated = isTruncated }
}
public enum GitHubItemState: String, Codable, Sendable, CaseIterable { case open, closed, all }
public enum GitHubMergeMethod: String, CaseIterable, Sendable, Identifiable {
    case merge, squash, rebase
    public var id: String { rawValue }
    public var title: String {
        switch self {
        case .merge: GHC("Merge commit")
        case .squash: GHC("Squash and merge")
        case .rebase: GHC("Rebase and merge")
        }
    }
    public var repositorySetting: String {
        switch self { case .merge: "allow_merge_commit"; case .squash: "allow_squash_merge"; case .rebase: "allow_rebase_merge" }
    }
}
public struct GitHubMergeProposal: Sendable, Equatable {
    public let method: GitHubMergeMethod
    public let repository: String
    public let number: Int
    public let headSHA: String
    public let baseRef: String
    public let baseRepository: String
}
