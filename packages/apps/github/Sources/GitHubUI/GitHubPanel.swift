import GitHubCore
import SwiftUI

public struct GitHubPanel: View {
    @Bindable private var service: GitHubService
    @State private var showsConnection = false
    @State private var showsMerge = false
    @State private var commentDrafts: [String: String] = [:]
    @FocusState private var searchFocused: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    public init(service: GitHubService) { self.service = service }

    public var body: some View {
        Group {
            if service.user == nil { connection }
            else {
                HSplitView {
                    if service.sidebarVisible {
                        sidebar.frame(minWidth: 210, idealWidth: CGFloat(service.sidebarWidth), maxWidth: 340)
                            .onGeometryChange(for: CGFloat.self, of: { $0.size.width }) { width in
                                guard width.isFinite, abs(Double(width) - service.sidebarWidth) > 0.5 else { return }
                                service.sidebarWidth = Double(width)
                                Task { await service.onPresentationChange?() }
                            }
                    }
                    main.frame(minWidth: 340, maxWidth: .infinity, maxHeight: .infinity)
                        .background(GitHubPanelStyle.background)
                }
            }
        }
        .font(.system(size: 13))
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .task { await service.restore() }
        .onAppear {
            service.onUIAction = { action in
                switch action {
                case "focusSearch":
                    service.sidebarVisible = true; searchFocused = true
                    Task { await service.onPresentationChange?() }
                    return true
                case "toggleSidebar":
                    withAnimation(reduceMotion ? nil : .easeInOut(duration: 0.18)) { service.sidebarVisible.toggle() }
                    Task { await service.onPresentationChange?() }
                    return true
                case "merge": showsMerge = true; return true
                case "closeDetails":
                    if showsMerge {
                        if !service.isMerging { service.cancelMerge(); showsMerge = false }
                        return true
                    }
                    if showsConnection { showsConnection = false; return true }
                    return false
                default: return false
                }
            }
        }
        .onDisappear { service.onUIAction = nil }
        .onChange(of: service.user?["login"].text) { old, new in
            if old != new { commentDrafts.removeAll() }
        }
        .sheet(isPresented: $showsConnection) { GitHubConnectionView(service: service) }
        .sheet(isPresented: $showsMerge) { GitHubMergeView(service: service) }
        .environment(\.openURL, OpenURLAction { url in
            Task { try? await service.openURL(url) }
            return .handled
        })
        .accessibilityIdentifier("github.panel")
    }

    private var connection: some View {
        VStack(spacing: 16) {
            GitHubIcon(kind: .open, size: 38).foregroundStyle(.secondary)
            Text(GHL("GitHub, in Nora")).font(.title2.weight(.semibold))
            Text(GHL("Your issues, reviews, and pull requests in one place."))
                .foregroundStyle(.secondary).multilineTextAlignment(.center)
            Button(GHL("Connect GitHub")) { showsConnection = true }
                .buttonStyle(.borderedProminent).controlSize(.large)
            if let error = service.error { Text(error).foregroundStyle(.red).textSelection(.enabled) }
        }.padding(32).frame(maxWidth: .infinity, maxHeight: .infinity)
            .background(GitHubPanelStyle.background)
    }

    private var sidebar: some View {
        VStack(spacing: 0) {
            GitHubAccountHeader(login: service.user?.login ?? "GitHub") {
                GitHubAvatar(person: service.user ?? .null, size: 24)
            } actions: {
                if let user = service.user { Text("@" + user.login) }
                Button(GHL("Manage repository access")) { Task { await service.manageAccess() } }
                Button(GHL("Refresh")) { Task { await service.refreshFeed() } }
                Button(GHL("Reconnect Account")) { showsConnection = true }
                Divider()
                Button(GHL("Disconnect"), role: .destructive) { Task { await service.disconnect() } }
            }
            VStack(spacing: 5) {
                navigation(.issues, title: "Issues", icon: .issue)
                navigation(.pullRequests, title: "PR", icon: .open)
            }.padding(.horizontal, 8).padding(.bottom, 14)
            HStack(spacing: 6) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary).font(.system(size: 11))
                TextField(GHL("Search loaded items"), text: $service.query)
                    .textFieldStyle(.plain).focused($searchFocused)
            }
            .padding(.horizontal, 9).frame(height: 28)
            .background(.quaternary.opacity(0.45), in: RoundedRectangle(cornerRadius: 5))
            .padding(.horizontal, 8)
            HStack(spacing: 6) {
                Menu {
                    Button(GHL("Related to me")) { changeRepository("") }
                    Divider()
                    ForEach(repositoryNames, id: \.self) { repository in
                        Button(repository) { changeRepository(repository) }
                    }
                } label: {
                    Text(service.repository.isEmpty ? GHL("Related to me") : service.repository)
                        .font(.system(size: 11)).lineLimit(1).truncationMode(.middle)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }.menuStyle(.borderlessButton)
                Image(systemName: "line.3.horizontal.decrease").font(.system(size: 11)).foregroundStyle(.secondary)
                Text(GHL("Open")).font(.system(size: 11)).foregroundStyle(.secondary)
            }.padding(.horizontal, 10).frame(height: 34)
            Divider().padding(.horizontal, 8)
            itemList
        }
        .background { GitHubSidebarBackground() }
    }

    private func navigation(_ kind: GitHubKind, title: String, icon: GitHubIconKind) -> some View {
        Button {
            Task { await service.switchKind(kind) }
        } label: {
            HStack(spacing: 10) {
                GitHubIcon(kind: icon, size: 17).foregroundStyle(.secondary)
                Text(title).fontWeight(.medium)
                Spacer()
            }
            .padding(.horizontal, 11).frame(height: 32)
            .background(service.kind == kind ? Color.primary.opacity(0.08) : .clear, in: RoundedRectangle(cornerRadius: 7))
            .contentShape(RoundedRectangle(cornerRadius: 7))
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(service.kind == kind ? .isSelected : [])
        .accessibilityIdentifier("github.section.\(kind == .issues ? "issues" : "pullRequests")")
    }

    private var repositoryNames: [String] { Array(Set(service.repositories.map { $0["full_name"].text } + service.items.map(\.repositoryName))).filter { !$0.isEmpty }.sorted() }
    private var currentItems: [GitHubValue] { service.items.filter { $0.isPullRequest == (service.kind == .pullRequests) } }
    private var visibleItems: [GitHubValue] {
        let query = service.query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !query.isEmpty else { return currentItems }
        return currentItems.filter { $0.title.localizedCaseInsensitiveContains(query) || $0.repositoryName.localizedCaseInsensitiveContains(query) || String($0["number"].int ?? 0).contains(query) }
    }
    private func changeRepository(_ repository: String) {
        Task { await service.selectRepository(repository) }
    }
    private var itemList: some View {
        ScrollView {
            LazyVStack(spacing: 4) {
                if service.isLoadingFeed && currentItems.isEmpty {
                    GitHubSkeleton(rows: 9, sidebar: true)
                } else {
                    ForEach(visibleItems, id: \.itemKey) { item in
                        Button { Task { await service.selectItem(item) } } label: {
                            GitHubItemRow(item: item, selected: service.selectedItem?.itemKey == item.itemKey)
                        }
                        .buttonStyle(.plain).accessibilityIdentifier("github.item.\(item.itemKey)")
                    }
                    if visibleItems.isEmpty {
                        Text(service.query.isEmpty ? GHL("You're all caught up.") : GHL("No matching items."))
                            .foregroundStyle(.secondary).padding(.vertical, 28)
                    }
                    if service.moreFeed {
                        GitHubSkeleton(rows: 2, sidebar: true)
                            .task { await service.loadMoreFeed() }
                            .accessibilityIdentifier("github.feed.loadMore")
                    }
                }
                if let notice = service.feedNotice {
                    Text(notice).font(.system(size: 10)).foregroundStyle(.tertiary).padding(12).textSelection(.enabled)
                }
                if let error = service.error {
                    VStack(spacing: 8) {
                        Text(error).font(.caption).foregroundStyle(.secondary).textSelection(.enabled)
                        Button(GHL("Retry")) { Task { await service.refresh() } }.buttonStyle(.borderless)
                    }.padding(12)
                }
            }
            .padding(8)
            .frame(maxWidth: .infinity, alignment: .topLeading)
            .transaction { $0.animation = nil }
            .id(service.kind)
            .transition(.opacity)
        }
        .animation(reduceMotion ? nil : .easeInOut(duration: 0.18), value: service.kind)
        .accessibilityIdentifier("github.items")
    }

    @ViewBuilder private var main: some View {
        if let item = service.detail ?? service.selectedItem {
            GitHubItemDetailView(service: service, item: item, drafts: $commentDrafts)
                .id(item.itemKey)
        } else {
            VStack(spacing: 12) {
                GitHubIcon(kind: service.kind == .issues ? .issue : .open, size: 30).foregroundStyle(.tertiary)
                Text(service.kind == .issues ? GHL("Select an issue") : GHL("Select a pull request"))
                    .font(.system(size: 16, weight: .semibold))
                Text(GHL("Read the discussion, review changes, and hand work to Nora."))
                    .font(.callout).foregroundStyle(.secondary)
            }.frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }
}

private struct GitHubItemRow: View {
    let item: GitHubValue
    let selected: Bool
    private var owner: GitHubValue {
        if !item["repository"]["owner"].isNull { return item["repository"]["owner"] }
        return .object(["login": .string(String(item.repositoryName.split(separator: "/").first ?? ""))])
    }
    var body: some View {
        HStack(spacing: 9) {
            GitHubAvatar(person: owner, size: 23)
            VStack(alignment: .leading, spacing: 3) {
                Text(item.title).font(.system(size: 12, weight: .medium)).lineLimit(1).truncationMode(.tail)
                    .frame(maxWidth: .infinity, alignment: .leading)
                HStack(spacing: 4) {
                    Text("#\(item["number"].int ?? 0) · \(item.repositoryName)").lineLimit(1).truncationMode(.tail)
                    Spacer(minLength: 0)
                    GitHubIcon(kind: item.statusIcon, size: 12).foregroundStyle(item.statusColor)
                }.font(.system(size: 10)).foregroundStyle(.secondary)
            }
        }
        .padding(.horizontal, 10).padding(.vertical, 9)
        .background(selected ? Color.primary.opacity(0.09) : .clear, in: RoundedRectangle(cornerRadius: 7))
        .contentShape(RoundedRectangle(cornerRadius: 7)).help(item.title)
    }
}
