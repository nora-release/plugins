import GitHubCore
import SwiftUI

/// One issue or pull request on the board: what it is, who opened it, and a way to hand it to Nora.
struct GitHubBoardCard: View {
    let item: GitHubValue
    let selected: Bool
    let open: () -> Void
    let handOff: () -> Void
    @State private var hovering = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var comments: Int { item["comments"].int ?? 0 }
    private var number: Int { item["number"].int ?? 0 }

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            heading
            Text(item.title)
                .font(.system(size: 13.5, weight: .medium))
                .lineSpacing(2)
                .lineLimit(2)
                .truncationMode(.tail)
                .frame(maxWidth: .infinity, alignment: .leading)
                .fixedSize(horizontal: false, vertical: true)
            labels
            footer
        }
        .padding(.horizontal, 14).padding(.top, 13).padding(.bottom, 12)
        .background(hovering || selected ? GitHubBoardStyle.cardHover : GitHubBoardStyle.card,
                    in: RoundedRectangle(cornerRadius: GitHubBoardStyle.cornerRadius))
        .overlay {
            RoundedRectangle(cornerRadius: GitHubBoardStyle.cornerRadius)
                .strokeBorder(selected ? Color.accentColor.opacity(0.7) : hovering ? GitHubBoardStyle.cardBorderHover : GitHubBoardStyle.cardBorder,
                              lineWidth: selected ? 1.5 : 1)
        }
        .shadow(color: .black.opacity(hovering ? 0.16 : 0.08), radius: hovering ? 8 : 2, y: hovering ? 3 : 1)
        .contentShape(RoundedRectangle(cornerRadius: GitHubBoardStyle.cornerRadius))
        .onTapGesture(perform: open)
        .onHover { inside in
            withAnimation(reduceMotion ? nil : .easeOut(duration: 0.12)) { hovering = inside }
        }
        .help(item.title)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(item.isPullRequest ? GHL("Pull request") : GHL("Issue")) #\(number), \(item.title)")
        .accessibilityAddTraits(.isButton)
        .accessibilityAction(named: GHL("Open"), open)
        .accessibilityIdentifier("github.card.\(item.itemKey)")
    }

    private var heading: some View {
        HStack(spacing: 7) {
            GitHubIcon(kind: item.isPullRequest ? (item["draft"].bool == true ? .draft : .open) : .issue, size: 15)
                .foregroundStyle(item["draft"].bool == true ? Color.secondary : GitHubBoardStyle.open)
                .frame(width: 16, height: 16)
            Text(verbatim: "#\(number)")
                .font(.system(size: 11.5, weight: .medium, design: .monospaced))
            if let date = item["updated_at"].date ?? item["created_at"].date {
                Text(verbatim: "·")
                Text(GitHubDateText.short(date))
            }
        }
        .font(.system(size: 12))
        .foregroundStyle(.secondary)
        .lineLimit(1)
    }

    @ViewBuilder private var labels: some View {
        let shown = item.boardLabels()
        if !shown.labels.isEmpty {
            HStack(spacing: 5) {
                ForEach(Array(shown.labels.enumerated()), id: \.offset) { _, label in
                    HStack(spacing: 5) {
                        Circle().fill(GitHubBoardStyle.labelColor(label.color)).frame(width: 6, height: 6)
                        Text(label.name).lineLimit(1)
                    }
                    .font(.system(size: 11.5))
                    .foregroundStyle(.secondary)
                    .padding(.leading, 7).padding(.trailing, 8).frame(height: 20)
                    .background(GitHubBoardStyle.labelFill, in: Capsule())
                }
                if shown.more > 0 {
                    Text(verbatim: "+\(shown.more)").font(.system(size: 11.5)).foregroundStyle(.tertiary)
                }
            }
        }
    }

    private var footer: some View {
        HStack(spacing: 8) {
            GitHubAvatar(person: item.person, size: 20)
            Text(item.person.login)
                .font(.system(size: 12)).foregroundStyle(.secondary)
                .lineLimit(1).truncationMode(.tail)
            if comments > 0 {
                HStack(spacing: 4) {
                    Image(systemName: "bubble.left").font(.system(size: 10.5))
                    Text(String(comments)).monospacedDigit()
                }
                .font(.system(size: 12)).foregroundStyle(.tertiary)
                .accessibilityElement(children: .ignore)
                .accessibilityLabel(GHL("\(comments) comments"))
            }
            Spacer(minLength: 6)
            Button(action: handOff) {
                Label(GHL("Give to Nora"), systemImage: "sparkles")
                    .font(.system(size: 12, weight: .medium))
                    .labelStyle(.titleAndIcon)
                    .fixedSize()
                    .padding(.horizontal, 10).frame(height: 24)
                    .overlay(Capsule().strokeBorder(GitHubBoardStyle.cardBorderHover, lineWidth: 1))
                    .contentShape(Capsule())
            }
            .buttonStyle(.plain)
            .layoutPriority(1)
            .opacity(hovering || selected ? 1 : 0.85)
            .help(GHL("Give to Nora"))
            .accessibilityIdentifier("github.card.\(item.itemKey).handoff")
        }
        .padding(.top, 2)
    }
}

/// A card redraws only when its item or its highlight changes; selecting another card leaves it alone.
extension GitHubBoardCard: Equatable {
    nonisolated static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.selected == rhs.selected && lhs.item == rhs.item
    }
}
