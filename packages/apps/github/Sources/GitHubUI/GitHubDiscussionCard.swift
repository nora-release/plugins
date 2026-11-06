import GitHubCore
import SwiftUI

struct GitHubDiscussionCard: View {
    let value: GitHubValue
    let bodyText: String
    var isDescription = false
    var kind = GitHubConversationEntry.Kind.comment

    var body: some View {
        HStack(alignment: .top, spacing: 8) {
            GitHubAvatar(person: value.person, size: 32).padding(.top, 6)
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 8) {
                    Text(value.person.login).font(.system(size: 12, weight: .semibold)).lineLimit(1)
                    Spacer(minLength: 8)
                    GitHubDate(date: value.eventDate)
                    if let status = reviewStatus {
                        Label(status.title, systemImage: status.symbol)
                            .font(.system(size: 10, weight: .medium)).foregroundStyle(status.color)
                            .padding(.horizontal, 5).padding(.vertical, 2)
                            .background(status.color.opacity(0.09), in: RoundedRectangle(cornerRadius: 3))
                    } else if isDescription, let association = association {
                        Text(association).font(.system(size: 10)).foregroundStyle(.secondary)
                            .padding(.horizontal, 5).padding(.vertical, 2)
                            .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: 3))
                    }
                }
                .padding(.horizontal, 14).frame(minHeight: 40)
                .background(Color.primary.opacity(0.025))
                Divider().opacity(0.7)
                VStack(alignment: .leading, spacing: 12) {
                    if kind == .codeComment {
                        Text(codeLocation).font(.system(size: 11, design: .monospaced)).foregroundStyle(.secondary)
                            .textSelection(.enabled)
                        if !value["diff_hunk"].text.isEmpty {
                            GitHubReviewContext(patch: value["diff_hunk"].text, fileName: value["path"].text)
                        }
                    }
                    if bodyText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                        Text(isDescription ? GHL("No description provided.") : kind == .review ? GHL("Submitted a review without a comment.") : GHL("No comment provided."))
                            .font(.system(size: 12)).foregroundStyle(.secondary)
                    } else { GitHubMarkdownView(source: bodyText) }
                    reactions
                }
                .padding(14).frame(maxWidth: .infinity, alignment: .leading)
            }
            .background(Color.primary.opacity(0.008), in: RoundedRectangle(cornerRadius: 6))
            .clipShape(RoundedRectangle(cornerRadius: 6))
            .overlay { RoundedRectangle(cornerRadius: 6).strokeBorder(Color.primary.opacity(0.09)) }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityIdentifier("github.discussion.\(isDescription ? "description" : value.id)")
    }
    private var codeLocation: String {
        let line = value["line"].int ?? value["original_line"].int
        return value["path"].text + (line.map { " · \($0)" } ?? "")
    }
    private var association: String? {
        switch value["author_association"].text {
        case "OWNER": GHL("Owner")
        case "MEMBER": GHL("Member")
        case "COLLABORATOR": GHL("Collaborator")
        default: nil
        }
    }
    private var reviewStatus: (title: String, symbol: String, color: Color)? {
        guard kind == .review else { return nil }
        switch value["state"].text.uppercased() {
        case "APPROVED": return (GHL("Approved"), "checkmark.circle", .green)
        case "CHANGES_REQUESTED": return (GHL("Changes requested"), "exclamationmark.bubble", .orange)
        case "DISMISSED": return (GHL("Dismissed"), "minus.circle", .secondary)
        case "PENDING": return (GHL("Pending review"), "clock", .secondary)
        default: return (GHL("Reviewed"), "text.bubble", .secondary)
        }
    }
    @ViewBuilder private var reactions: some View {
        let keys = [("+1", "👍"), ("-1", "👎"), ("laugh", "😄"), ("hooray", "🎉"), ("confused", "😕"), ("heart", "❤️"), ("rocket", "🚀"), ("eyes", "👀")]
        let visible = keys.filter { (value["reactions"][$0.0].int ?? 0) > 0 }
        if !visible.isEmpty {
            HStack(spacing: 6) {
                ForEach(visible, id: \.0) { key, emoji in
                    Text("\(emoji) \(value["reactions"][key].int ?? 0)")
                        .font(.system(size: 11)).padding(.horizontal, 7).padding(.vertical, 3)
                        .background(.quaternary.opacity(0.4), in: Capsule())
                }
            }
        }
    }
}

struct GitHubEventRow: View {
    let entry: GitHubConversationEntry
    var body: some View {
        HStack(spacing: 9) {
            GitHubAvatar(person: entry.value.person, size: 18)
            Text(entry.title).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                .frame(maxWidth: .infinity, alignment: .leading).help(entry.title)
            if !entry.value.shortSHA.isEmpty {
                Text(entry.value.shortSHA).font(.system(size: 11, weight: .medium, design: .monospaced)).foregroundStyle(.secondary).textSelection(.enabled)
            }
            GitHubDate(date: entry.date)
        }
        .frame(minHeight: 28).padding(.horizontal, 3)
    }
}

private struct GitHubReviewContext: View {
    let patch: String
    let fileName: String
    var body: some View {
        GitHubNativeDiff(patch: patch, fileName: fileName, layout: .unified)
            .frame(height: min(176, CGFloat(patch.split(separator: "\n").count) * 22 + 8))
            .clipShape(RoundedRectangle(cornerRadius: 4))
            .overlay { RoundedRectangle(cornerRadius: 4).strokeBorder(Color.primary.opacity(0.06)) }
    }
}
