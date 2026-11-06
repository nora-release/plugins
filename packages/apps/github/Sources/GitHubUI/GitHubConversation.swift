import Foundation
import GitHubCore

struct GitHubConversationEntry: Identifiable, Equatable, Sendable {
    enum Kind: Equatable, Sendable { case comment, review, codeComment, commit, forcePush, event }
    let id: String
    let kind: Kind
    let value: GitHubValue
    let date: Date?
    init(id: String, kind: Kind, value: GitHubValue) {
        self.id = id; self.kind = kind; self.value = value; date = value.eventDate
    }
    var isCommitUpdate: Bool { kind == .commit || kind == .forcePush }
    var title: String {
        if kind == .commit { return value.commitTitle }
        switch value["event"].text {
        case "head_ref_force_pushed": return GHL("Force-pushed the source branch")
        case "merged": return GHL("Merged this pull request")
        case "closed": return GHL("Closed this item")
        case "reopened": return GHL("Reopened this item")
        case "ready_for_review": return GHL("Marked as ready for review")
        case "convert_to_draft": return GHL("Converted to draft")
        case "renamed": return GHL("Changed the title") + " · " + value["rename"]["to"].text
        case "labeled": return GHL("Added label") + " · " + value["label"]["name"].text
        case "unlabeled": return GHL("Removed label") + " · " + value["label"]["name"].text
        case "assigned": return GHL("Assigned") + " · " + value["assignee"].login
        case "unassigned": return GHL("Unassigned") + " · " + value["assignee"].login
        case "review_requested": return GHL("Requested a review") + " · " + value["requested_reviewer"].login
        case "review_request_removed": return GHL("Removed a review request")
        case "review_dismissed": return GHL("Dismissed a review")
        case "head_ref_deleted": return GHL("Deleted the source branch")
        case "head_ref_restored": return GHL("Restored the source branch")
        case "base_ref_changed": return GHL("Changed the base branch")
        case "base_ref_force_pushed": return GHL("Force-pushed the base branch")
        case "referenced", "cross-referenced":
            let issue = value["source"]["issue"]
            return GHL("Referenced in") + " #\(issue["number"].int ?? 0) " + issue.title
        case "milestoned": return GHL("Added to milestone") + " · " + value["milestone"]["title"].text
        case "demilestoned": return GHL("Removed from milestone")
        case "locked": return GHL("Locked this conversation")
        case "unlocked": return GHL("Unlocked this conversation")
        case "pinned": return GHL("Pinned this item")
        case "unpinned": return GHL("Unpinned this item")
        case "mentioned": return GHL("Mentioned this item")
        case "subscribed": return GHL("Subscribed to this item")
        case "unsubscribed": return GHL("Unsubscribed from this item")
        case "auto_merge_enabled": return GHL("Enabled automatic merge")
        case "auto_merge_disabled": return GHL("Disabled automatic merge")
        case "added_to_merge_queue": return GHL("Added to merge queue")
        case "removed_from_merge_queue": return GHL("Removed from merge queue")
        case "connected": return GHL("Connected a related item")
        case "disconnected": return GHL("Disconnected a related item")
        case "transferred": return GHL("Transferred this item")
        default:
            return value["event"].text.replacingOccurrences(of: "_", with: " ").capitalized
        }
    }
}

struct GitHubConversationGroup: Identifiable, Equatable, Sendable {
    let entries: [GitHubConversationEntry]
    var id: String { entries.first?.id ?? "empty" }
    var isCollection: Bool { entries.count > 1 && entries.allSatisfy(\.isCommitUpdate) }
    var containsForcePush: Bool { entries.contains { $0.kind == .forcePush } }
}

enum GitHubConversation {
    static func groups(sections: [GitHubSection: GitHubSectionState]) -> [GitHubConversationGroup] {
        var entries: [GitHubConversationEntry] = []
        var seen = Set<String>()
        var commitSHAs = Set<String>()
        let commits = Dictionary((sections[.commits]?.items ?? []).map { ($0["sha"].text.lowercased(), $0) }, uniquingKeysWith: { first, _ in first })
        func append(_ original: GitHubValue, kind: GitHubConversationEntry.Kind, prefix: String) {
            var value = original
            if kind == .commit, let commit = commits[(value["sha"].string ?? value["commit_id"].text).lowercased()] {
                if !commit["author"].isNull { value = value.setting("user", commit["author"]) }
                if value["commit"].isNull { value = value.setting("commit", commit["commit"]) }
                if value["message"].text.isEmpty { value = value.setting("message", commit["commit"]["message"]) }
            }
            let identity = kind == .commit ? (value["sha"].string ?? value["commit_id"].text).lowercased() : value.id
            guard !identity.isEmpty else { return }
            let id = prefix + ":" + identity
            guard seen.insert(id).inserted else { return }
            if kind == .commit { commitSHAs.insert(identity) }
            entries.append(.init(id: id, kind: kind, value: value))
        }
        for value in sections[.comments]?.items ?? [] { append(value, kind: .comment, prefix: "comment") }
        for value in sections[.reviews]?.items ?? [] { append(value, kind: .review, prefix: "review") }
        for value in sections[.reviewComments]?.items ?? [] { append(value, kind: .codeComment, prefix: "code-comment") }
        for value in sections[.timeline]?.items ?? [] {
            switch value["event"].text {
            case "commented": append(value, kind: .comment, prefix: "comment")
            case "reviewed": append(value, kind: .review, prefix: "review")
            case "committed": append(value, kind: .commit, prefix: "commit")
            case "head_ref_force_pushed": append(value, kind: .forcePush, prefix: "event")
            default: append(value, kind: .event, prefix: "event")
            }
        }
        for value in sections[.commits]?.items ?? [] where !commitSHAs.contains(value["sha"].text.lowercased()) {
            append(value, kind: .commit, prefix: "commit")
        }
        entries.sort {
            if $0.date != $1.date { return ($0.date ?? .distantPast) < ($1.date ?? .distantPast) }
            return $0.id < $1.id
        }
        var output: [GitHubConversationGroup] = []
        var pending: [GitHubConversationEntry] = []
        func flush() {
            if !pending.isEmpty { output.append(.init(entries: pending)); pending.removeAll() }
        }
        for entry in entries {
            if entry.isCommitUpdate { pending.append(entry) }
            else { flush(); output.append(.init(entries: [entry])) }
        }
        flush()
        return output
    }
}

struct GitHubReviewer: Identifiable, Equatable, Sendable {
    let person: GitHubValue
    let state: String
    var id: String { person.login.lowercased() }
}

enum GitHubReviewers {
    static func latest(reviews: [GitHubValue], requested: [GitHubValue]) -> [GitHubReviewer] {
        var values: [String: GitHubReviewer] = [:]
        let ordered = reviews.map { ($0, $0.eventDate ?? .distantPast) }.sorted { $0.1 < $1.1 }
        for (review, _) in ordered {
            let person = review["user"], key = person.login.lowercased(), state = review["state"].text.uppercased()
            guard !key.isEmpty else { continue }
            if state == "COMMENTED", let prior = values[key], ["APPROVED", "CHANGES_REQUESTED", "DISMISSED"].contains(prior.state) { continue }
            values[key] = .init(person: person, state: state)
        }
        for person in requested where !person.login.isEmpty { values[person.login.lowercased()] = .init(person: person, state: "REQUESTED") }
        return values.values.sorted { $0.id < $1.id }
    }
}
