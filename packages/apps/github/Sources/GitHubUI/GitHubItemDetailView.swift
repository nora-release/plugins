import GitHubCore
import SwiftUI

enum GitHubDetailTab: String, CaseIterable {
    case discussion, commits, checks, files
    var title: String {
        switch self {
        case .discussion: GHL("Conversation")
        case .commits: GHL("Commits")
        case .checks: GHL("Checks")
        case .files: GHL("Files changed")
        }
    }
    var icon: GitHubIconKind { switch self { case .discussion: .discussion; case .commits: .commit; case .checks: .checks; case .files: .files } }
}

struct GitHubItemDetailView: View {
    @Bindable var service: GitHubService
    let item: GitHubValue
    @Binding var drafts: [String: String]
    @State private var tab = GitHubDetailTab.discussion
    @State private var expandedGroups = Set<String>()
    @State private var conversationGroups: [GitHubConversationGroup] = []

    var body: some View {
        VStack(spacing: 0) {
            tabs.padding(.top, 12).padding(.horizontal, 24)
            Group {
                if service.isLoadingDetail {
                    GitHubDetailColumns {
                        GitHubSkeleton(rows: 4, inset: 0)
                    } aside: {
                        GitHubItemAside(item: item, reviews: [])
                    }
                } else if let error = service.detailError {
                    VStack(spacing: 12) {
                        Text(error).foregroundStyle(.secondary).textSelection(.enabled)
                        Button(GHL("Retry")) { Task { await service.selectItem(item) } }
                    }.padding(32).frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    switch tab {
                    case .discussion: discussion
                    case .commits: commitList
                    case .checks: GitHubChecksView(service: service)
                    case .files: GitHubFilesView(service: service)
                    }
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .task(id: tab) {
            switch tab {
            case .discussion: break
            case .commits: await service.loadAll(.commits)
            case .checks:
                await service.loadAll(.checks); await service.loadAll(.statuses); await service.loadAll(.workflows)
            case .files: await service.loadAll(.files)
            }
        }
        .task(id: conversationInputs) {
            let snapshot = service.sections
            let groups = await Task.detached(priority: .userInitiated) { GitHubConversation.groups(sections: snapshot) }.value
            guard !Task.isCancelled else { return }
            conversationGroups = groups
        }
        .accessibilityIdentifier("github.details")
    }

    private var conversationInputs: [[GitHubValue]] {
        [GitHubSection.comments, .reviews, .reviewComments, .timeline, .commits].map { service.sections[$0]?.items ?? [] }
    }
    private var tabs: some View {
        HStack(spacing: 0) {
            ForEach(item.isPullRequest ? GitHubDetailTab.allCases : [.discussion], id: \.self) { value in
                Button { tab = value } label: {
                    HStack(spacing: 6) {
                        GitHubIcon(kind: value.icon, size: 15)
                        Text(value.title).fontWeight(tab == value ? .semibold : .medium)
                        Text(String(count(value))).font(.system(size: 10)).monospacedDigit()
                            .padding(.horizontal, 5).padding(.vertical, 1)
                            .background(.quaternary.opacity(0.55), in: Capsule())
                    }
                    .font(.system(size: 12))
                    .padding(.horizontal, 12).frame(height: 36)
                    .background(tab == value ? Color.primary.opacity(0.025) : .clear)
                    .overlay(alignment: .bottom) { if tab == value { Rectangle().fill(Color.primary.opacity(0.65)).frame(height: 1) } }
                }
                .buttonStyle(.plain).foregroundStyle(tab == value ? .primary : .secondary)
                .accessibilityAddTraits(tab == value ? .isSelected : [])
                .accessibilityIdentifier("github.tab.\(value.rawValue)")
            }
            Spacer(minLength: 8)
            if item.isPullRequest { GitHubChangeCount(item: item) }
        }
        .overlay(alignment: .bottom) { Rectangle().fill(Color.primary.opacity(0.1)).frame(height: 1) }
    }
    private func count(_ tab: GitHubDetailTab) -> Int {
        switch tab {
        case .discussion: item["comments"].int ?? service.sections[.comments]?.items.count ?? 0
        case .commits: item["commits"].int ?? service.sections[.commits]?.items.count ?? 0
        case .checks: (service.sections[.checks]?.items.count ?? 0) + (service.sections[.statuses]?.items.count ?? 0)
        case .files: item["changed_files"].int ?? service.sections[.files]?.items.count ?? 0
        }
    }
    private var discussion: some View {
        GitHubDetailColumns {
            VStack(alignment: .leading, spacing: 12) {
                GitHubDiscussionCard(value: item, bodyText: item["body"].text, isDescription: true)
                ForEach(conversationGroups) { group in
                    if group.isCollection {
                        commitGroup(group)
                    } else if let entry = group.entries.first {
                        conversationEntry(entry)
                    }
                }
                discussionLoading
                GitHubCommentComposer(service: service, draft: Binding(
                    get: { drafts[item.itemKey] ?? "" },
                    set: { drafts[item.itemKey] = $0 }
                ))
            }
        } aside: {
            GitHubItemAside(item: item, reviews: service.sections[.reviews]?.items ?? [])
        }
    }
    @ViewBuilder private var discussionLoading: some View {
        let names: [GitHubSection] = item.isPullRequest ? [.comments, .reviews, .reviewComments, .timeline] : [.comments, .timeline]
        if names.contains(where: { service.sections[$0]?.loading == true }) {
            GitHubSkeleton(rows: 1).padding(.leading, 40)
        }
        ForEach(names, id: \.self) { name in
            if let error = service.sections[name]?.error {
                HStack {
                    Text(error).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                    Spacer()
                    Button(GHL("Retry")) { Task { await service.loadSection(name) } }
                }.padding(.leading, 40)
            }
        }
    }
    private func commitGroup(_ group: GitHubConversationGroup) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Button {
                if !expandedGroups.insert(group.id).inserted { expandedGroups.remove(group.id) }
            } label: {
                HStack(spacing: 12) {
                    GitHubIcon(kind: .commit, size: 14).foregroundStyle(.secondary)
                    Text(group.containsForcePush ? GHL("\(group.entries.count) commit updates") : GHL("\(group.entries.count) commits"))
                    Spacer()
                    GitHubDate(date: group.entries.last?.date, relative: true)
                    Image(systemName: expandedGroups.contains(group.id) ? "chevron.down" : "chevron.right")
                        .font(.system(size: 9)).foregroundStyle(.tertiary)
                }
                .font(.system(size: 12, weight: .medium)).padding(.horizontal, 12).frame(height: 36)
                .background(Color.primary.opacity(0.015), in: RoundedRectangle(cornerRadius: 6))
                .overlay { RoundedRectangle(cornerRadius: 6).strokeBorder(Color.primary.opacity(0.08)) }
                .contentShape(RoundedRectangle(cornerRadius: 6))
            }
            .buttonStyle(.plain).accessibilityIdentifier("github.commits.group.\(group.id)")
            if expandedGroups.contains(group.id) {
                ForEach(group.entries) { entry in GitHubEventRow(entry: entry) }
            }
        }
        .padding(.leading, 40)
    }
    @ViewBuilder private func conversationEntry(_ entry: GitHubConversationEntry) -> some View {
        switch entry.kind {
        case .comment, .review, .codeComment:
            GitHubDiscussionCard(value: entry.value, bodyText: entry.value["body"].text, kind: entry.kind)
        default:
            GitHubEventRow(entry: entry).padding(.leading, 40)
        }
    }
    private var commitList: some View {
        ScrollView {
            LazyVStack(spacing: 0) {
                if service.sections[.commits]?.loading == true && service.sections[.commits]?.items.isEmpty != false { GitHubSkeleton(rows: 5) }
                ForEach(service.sections[.commits]?.items ?? [], id: \.id) { value in
                    HStack(alignment: .top, spacing: 10) {
                        GitHubAvatar(person: value.person, size: 25)
                        VStack(alignment: .leading, spacing: 5) {
                            Text(value.commitTitle).font(.system(size: 13, weight: .medium)).textSelection(.enabled)
                            Text(value.person.login).font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer(minLength: 12)
                        Text(value.shortSHA).font(.system(size: 11, design: .monospaced)).foregroundStyle(.secondary)
                        GitHubDate(date: value.eventDate)
                    }.padding(14)
                    Divider()
                }
                GitHubSectionError(service: service, section: .commits)
            }.padding(24)
        }
    }
}

private struct GitHubDetailColumns<Content: View, Aside: View>: View {
    @ViewBuilder let content: Content
    @ViewBuilder let aside: Aside

    var body: some View {
        GeometryReader { geometry in
            ScrollView {
                HStack(alignment: .top, spacing: 24) {
                    content.frame(maxWidth: .infinity, alignment: .leading)
                    if geometry.size.width >= 820 { aside.frame(width: 210) }
                }
                .padding(.horizontal, 24).padding(.top, 18).padding(.bottom, 24)
                .frame(maxWidth: .infinity, alignment: .topLeading)
            }
        }
    }
}

struct GitHubSectionError: View {
    @Bindable var service: GitHubService
    let section: GitHubSection
    var body: some View {
        if let error = service.sections[section]?.error {
            HStack {
                Text(error).foregroundStyle(.secondary).textSelection(.enabled)
                Spacer()
                Button(GHL("Retry")) { Task { await service.loadAll(section) } }
            }.font(.caption).padding(12)
        }
    }
}

private struct GitHubItemAside: View {
    let item: GitHubValue
    let reviews: [GitHubValue]
    private var reviewers: [GitHubReviewer] { GitHubReviewers.latest(reviews: reviews, requested: item["requested_reviewers"].array) }
    private var assignees: [GitHubValue] {
        if !item["assignees"].isNull { return item["assignees"].array }
        return item["assignee"].isNull ? [] : [item["assignee"]]
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            VStack(alignment: .leading, spacing: 8) {
                caption(GHL("Reviewers"))
                if reviewers.isEmpty && item["requested_teams"].array.isEmpty { empty(GHL("No reviewers")) }
                ForEach(reviewers) { reviewer in
                    HStack(spacing: 7) {
                        GitHubAvatar(person: reviewer.person, size: 18)
                        Text(reviewer.person.login).font(.system(size: 12))
                        Spacer(minLength: 3)
                        Image(systemName: reviewer.state == "APPROVED" ? "checkmark.circle" : reviewer.state == "CHANGES_REQUESTED" ? "exclamationmark.bubble" : "circle")
                            .font(.system(size: 11)).foregroundStyle(reviewer.state == "APPROVED" ? .green : reviewer.state == "CHANGES_REQUESTED" ? .orange : .secondary)
                            .help(reviewer.state)
                    }
                }
                ForEach(item["requested_teams"].array, id: \.id) { team in
                    Label(team["name"].string ?? team["slug"].text, systemImage: "person.2").font(.system(size: 12))
                }
            }
            Divider()
            people(GHL("Assignees"), values: assignees, empty: GHL("No assignees"))
            Divider()
            VStack(alignment: .leading, spacing: 8) {
                caption(GHL("Labels"))
                if item["labels"].array.isEmpty { empty(GHL("No labels")) }
                ForEach(item["labels"].array, id: \.self) { label in
                    Text(label["name"].string ?? label.text).font(.system(size: 11, weight: .medium)).padding(.horizontal, 8).padding(.vertical, 3)
                        .background(labelColor(label).opacity(0.14), in: RoundedRectangle(cornerRadius: 4)).foregroundStyle(labelColor(label))
                }
            }
            Divider()
            VStack(alignment: .leading, spacing: 8) {
                caption(GHL("Milestone"))
                if item["milestone"]["title"].text.isEmpty { empty(GHL("No milestone")) }
                else { Text(item["milestone"]["title"].text).font(.system(size: 12)) }
            }
        }
    }
    private func caption(_ title: String) -> some View { Text(title).font(.system(size: 11, weight: .medium)).foregroundStyle(.secondary) }
    private func empty(_ title: String) -> some View { Text(title).font(.system(size: 11)).foregroundStyle(.tertiary) }
    private func people(_ title: String, values: [GitHubValue], empty placeholder: String) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            caption(title)
            if values.isEmpty { empty(placeholder) }
            ForEach(values, id: \.id) { person in
                HStack(spacing: 7) { GitHubAvatar(person: person, size: 18); Text(person.login).font(.system(size: 12)) }
            }
        }
    }
    private func labelColor(_ value: GitHubValue) -> Color {
        guard let number = UInt32(value["color"].text, radix: 16) else { return .secondary }
        return Color(red: Double((number >> 16) & 255) / 255, green: Double((number >> 8) & 255) / 255, blue: Double(number & 255) / 255)
    }
}
