import GitHubCore
import SwiftUI

/// The open issues and pull requests related to the user, one column per repository.
struct GitHubBoardView: View {
    @Bindable var service: GitHubService
    @Binding var filter: GitHubBoardFilter
    var searchFocused: FocusState<Bool>.Binding
    let reconnect: () -> Void
    let handOff: (GitHubValue) -> Void

    private var columns: [GitHubBoardColumn] {
        GitHubBoardColumn.columns(service.items, filter: filter, query: service.query)
    }

    var body: some View {
        VStack(spacing: 0) {
            header
            Rectangle().fill(GitHubBoardStyle.divider).frame(height: 1)
            if let error = service.error {
                banner(error, retry: true)
            } else if let notice = service.feedNotice {
                banner(notice, retry: false)
            }
            content
        }
        .background(GitHubBoardStyle.background)
    }

    // MARK: Header

    private var header: some View {
        HStack(spacing: 14) {
            filters
            Spacer(minLength: 8)
            search
            GitHubBoardAccount(service: service, reconnect: reconnect)
        }
        .padding(.horizontal, 24).frame(height: 56)
    }

    private var filters: some View {
        HStack(spacing: 2) {
            ForEach(GitHubBoardFilter.allCases) { option in
                let count = service.items.filter(option.includes).count
                Button { filter = option } label: {
                    HStack(spacing: 5) {
                        Text(option.title).fontWeight(.medium)
                        Text(String(count)).monospacedDigit().foregroundStyle(.secondary)
                    }
                    .font(.system(size: 12.5))
                    .foregroundStyle(filter == option ? .primary : .secondary)
                    .padding(.horizontal, 11).frame(height: 26)
                    .background {
                        if filter == option {
                            RoundedRectangle(cornerRadius: 7).fill(GitHubBoardStyle.selectedSegment)
                                .shadow(color: .black.opacity(0.12), radius: 1, y: 0.5)
                        }
                    }
                    .contentShape(RoundedRectangle(cornerRadius: 7))
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(filter == option ? .isSelected : [])
                .accessibilityIdentifier("github.board.filter.\(option.rawValue)")
            }
        }
        .padding(3)
        .background(GitHubBoardStyle.labelFill, in: RoundedRectangle(cornerRadius: 9))
    }

    private var search: some View {
        HStack(spacing: 7) {
            Image(systemName: "magnifyingglass").font(.system(size: 11.5)).foregroundStyle(.tertiary)
            TextField(GHL("Search"), text: $service.query)
                .textFieldStyle(.plain)
                .focused(searchFocused)
                .accessibilityIdentifier("github.board.search")
        }
        .font(.system(size: 12.5))
        .padding(.horizontal, 10).frame(width: 220, height: 30)
        .background(GitHubBoardStyle.control, in: RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(GitHubBoardStyle.controlBorder))
    }

    private func banner(_ text: String, retry: Bool) -> some View {
        HStack(spacing: 10) {
            Image(systemName: retry ? "exclamationmark.triangle" : "info.circle").foregroundStyle(.secondary)
            Text(text).foregroundStyle(.secondary).textSelection(.enabled).lineLimit(2)
            Spacer(minLength: 8)
            if retry {
                Button(GHL("Retry")) { Task { await service.refreshFeed() } }.buttonStyle(.borderless)
            }
        }
        .font(.system(size: 12))
        .padding(.horizontal, 24).padding(.vertical, 9)
        .background(GitHubBoardStyle.labelFill.opacity(0.6))
    }

    // MARK: Columns

    @ViewBuilder private var content: some View {
        if service.isLoadingFeed && service.items.isEmpty {
            loadingColumns
        } else if columns.isEmpty {
            empty
        } else {
            ScrollView(.horizontal) {
                HStack(alignment: .top, spacing: 16) {
                    ForEach(columns) { column in
                        GitHubBoardColumnView(column: column, selectedKey: service.selectedItem?.itemKey,
                                              open: { item in Task { await service.selectItem(item) } },
                                              handOff: handOff)
                    }
                    if service.moreFeed {
                        ProgressView().controlSize(.small)
                            .frame(width: 60).padding(.top, 40)
                            .task { await service.loadMoreFeed() }
                            .accessibilityIdentifier("github.board.loadMore")
                    }
                }
                .padding(.horizontal, 24).padding(.top, 18)
                .frame(maxHeight: .infinity, alignment: .top)
            }
            .scrollIndicators(.automatic)
            .accessibilityIdentifier("github.board")
        }
    }

    private var loadingColumns: some View {
        HStack(alignment: .top, spacing: 16) {
            ForEach(0 ..< 3, id: \.self) { _ in
                VStack(alignment: .leading, spacing: 10) {
                    RoundedRectangle(cornerRadius: 5).fill(GitHubBoardStyle.labelFill).frame(width: 140, height: 14).padding(.vertical, 6)
                    ForEach(0 ..< 3, id: \.self) { _ in
                        RoundedRectangle(cornerRadius: GitHubBoardStyle.cornerRadius).fill(GitHubBoardStyle.card).frame(height: 116)
                            .overlay(RoundedRectangle(cornerRadius: GitHubBoardStyle.cornerRadius).strokeBorder(GitHubBoardStyle.cardBorder))
                    }
                }
                .frame(width: GitHubBoardStyle.columnWidth)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 24).padding(.top, 18)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .accessibilityLabel(GHL("Loading"))
    }

    private var empty: some View {
        VStack(spacing: 12) {
            GitHubIcon(kind: .issue, size: 30).foregroundStyle(.tertiary)
            Text(service.query.isEmpty ? GHL("Nothing needs you right now.") : GHL("No matching items."))
                .font(.system(size: 15, weight: .semibold))
            if service.query.isEmpty {
                Text(GHL("Open issues and pull requests that involve you show up here."))
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// One repository's column: its name and how many items, then its cards.
private struct GitHubBoardColumnView: View {
    let column: GitHubBoardColumn
    let selectedKey: String?
    let open: (GitHubValue) -> Void
    let handOff: (GitHubValue) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 9) {
                GitHubRepositoryOwnerAvatar(column: column)
                (Text(column.owner + "/").foregroundStyle(.secondary).fontWeight(.regular) + Text(column.name))
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(1).truncationMode(.middle)
                Text(String(column.items.count)).font(.system(size: 12)).monospacedDigit().foregroundStyle(.tertiary)
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 4).frame(height: 30)
            .help(column.repository)
            .accessibilityElement(children: .combine)
            .accessibilityAddTraits(.isHeader)
            ScrollView(.vertical) {
                LazyVStack(spacing: 10) {
                    ForEach(column.items, id: \.itemKey) { item in
                        GitHubBoardCard(item: item, selected: selectedKey == item.itemKey,
                                        open: { open(item) }, handOff: { handOff(item) })
                    }
                }
                .padding(.bottom, 24).padding(.horizontal, 1).padding(.top, 1)
            }
            .scrollIndicators(.never)
        }
        .frame(width: GitHubBoardStyle.columnWidth)
        .accessibilityElement(children: .contain)
        .accessibilityLabel(column.repository)
        .accessibilityIdentifier("github.board.column.\(column.id)")
    }
}

/// The signed-in account and its actions. The avatar sits beside the menu, never in its label:
/// a menu's label image becomes the native menu's image at the picture's own size.
struct GitHubBoardAccount: View {
    let service: GitHubService
    let reconnect: () -> Void

    var body: some View {
        HStack(spacing: 7) {
            GitHubAvatar(person: service.user ?? .null, size: 24)
                .frame(width: 24, height: 24).clipShape(Circle())
                .accessibilityHidden(true)
            Menu {
                Button(GHL("Manage repository access")) { Task { await service.manageAccess() } }
                Button(GHL("Reconnect Account"), action: reconnect)
                Divider()
                Button(GHL("Disconnect"), role: .destructive) { Task { await service.disconnect() } }
            } label: {
                Text(service.user?.login ?? "GitHub")
                    .font(.system(size: 12.5, weight: .medium)).lineLimit(1)
                    .frame(minHeight: 28, maxHeight: 28)
                    .contentShape(Rectangle())
            }
            .menuStyle(.borderlessButton).menuIndicator(.visible).fixedSize()
            .frame(height: 28)
            .accessibilityLabel(GHL("Account"))
            .accessibilityIdentifier("github.board.account")
        }
        .frame(height: 30)
    }
}

/// The picture of the account that owns a repository, a person or an organization, in a rounded square;
/// the repository's tinted initial stands in until it loads, or when it can't.
struct GitHubRepositoryOwnerAvatar: View {
    let column: GitHubBoardColumn
    var size: CGFloat = 22

    var url: URL? {
        guard !column.owner.isEmpty, column.owner.allSatisfy({ $0.isLetter || $0.isNumber || $0 == "-" }) else { return nil }
        return URL(string: "https://github.com/\(column.owner).png?size=64")
    }

    var body: some View {
        AsyncImage(url: url) { phase in
            if case let .success(image) = phase {
                image.resizable().scaledToFill()
            } else {
                Text(String(column.name.first { $0.isLetter || $0.isNumber } ?? "#").uppercased())
                    .font(.system(size: size * 0.5, weight: .semibold))
                    .foregroundStyle(Color.black.opacity(0.78))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .background(GitHubBoardStyle.tint(for: column.repository))
            }
        }
        .frame(width: size, height: size)
        .clipShape(RoundedRectangle(cornerRadius: size * 0.27))
        .overlay(RoundedRectangle(cornerRadius: size * 0.27).strokeBorder(GitHubBoardStyle.cardBorder))
        .help(column.owner)
        .accessibilityHidden(true)
    }
}
