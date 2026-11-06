import GitHubCore
import SwiftUI

struct GitHubMergeView: View {
    @Bindable var service: GitHubService
    @State private var method = GitHubMergeMethod.merge
    @Environment(\.dismiss) private var dismiss
    private var allowed: [GitHubMergeMethod] {
        GitHubMergeMethod.allCases.filter { service.sections[.repository]?.data[$0.repositorySetting].bool == true }
    }
    var body: some View {
        VStack(alignment: .leading, spacing: 20) {
            HStack(spacing: 10) {
                GitHubIcon(kind: .merged, size: 23).foregroundStyle(.purple)
                Text(GHL("Merge pull request")).font(.title3.weight(.semibold))
            }
            if let item = service.detail {
                VStack(alignment: .leading, spacing: 5) {
                    Text(item.title + " #\(item["number"].int ?? 0)").font(.system(size: 14, weight: .semibold))
                    Text(item["head"]["ref"].text + " → " + item["base"]["ref"].text)
                        .font(.system(size: 11, design: .monospaced)).foregroundStyle(.secondary)
                }
            }
            if let proposal = service.mergeProposal {
                VStack(alignment: .leading, spacing: 10) {
                    Label(proposal.method.title, systemImage: "checkmark.circle").fontWeight(.medium)
                    Text(GHL("This will merge commit \(String(proposal.headSHA.prefix(8))) into \(proposal.baseRef)."))
                        .foregroundStyle(.secondary)
                    Text(proposal.repository).font(.caption).foregroundStyle(.secondary)
                }
                .padding(14).frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.purple.opacity(0.06), in: RoundedRectangle(cornerRadius: 6))
            } else {
                VStack(spacing: 8) {
                    ForEach(allowed) { value in
                        Button { method = value } label: {
                            HStack(alignment: .top, spacing: 10) {
                                Image(systemName: method == value ? "largecircle.fill.circle" : "circle").foregroundStyle(method == value ? .purple : .secondary)
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(value.title).font(.system(size: 13, weight: .medium))
                                    Text(explanation(value)).font(.system(size: 11)).foregroundStyle(.secondary)
                                }
                                Spacer()
                            }.padding(12).contentShape(Rectangle())
                                .background(method == value ? Color.purple.opacity(0.05) : .clear, in: RoundedRectangle(cornerRadius: 6))
                                .overlay { RoundedRectangle(cornerRadius: 6).strokeBorder(method == value ? Color.purple.opacity(0.3) : Color.primary.opacity(0.08)) }
                        }.buttonStyle(.plain)
                    }
                }
            }
            if let error = service.mergeError { Text(error).font(.callout).foregroundStyle(.red).textSelection(.enabled) }
            if allowed.isEmpty, service.mergeProposal == nil {
                Text(GHL("No merge methods are available for this repository.")).foregroundStyle(.secondary)
            }
            HStack {
                Button(GHL("Cancel")) { service.cancelMerge(); dismiss() }.keyboardShortcut(.cancelAction).disabled(service.isMerging)
                Spacer()
                if service.mergeProposal != nil {
                    Button {
                        Task { await service.confirmMerge(); if service.detail?["merged"].bool == true { dismiss() } }
                    } label: {
                        HStack(spacing: 6) {
                            if service.isMerging { ProgressView().controlSize(.mini) }
                            Text(GHL("Confirm merge"))
                        }
                    }.buttonStyle(.borderedProminent).tint(.purple).disabled(service.isMerging)
                } else {
                    Button(GHL("Continue")) { Task { await service.prepareMerge(method) } }
                        .buttonStyle(.borderedProminent).tint(.purple).disabled(allowed.isEmpty)
                }
            }
        }
        .padding(24).frame(width: 460)
        .onAppear { if let first = allowed.first { method = first } }
        .interactiveDismissDisabled(service.isMerging)
        .onDisappear { service.cancelMerge() }
    }
    private func explanation(_ method: GitHubMergeMethod) -> String {
        switch method {
        case .merge: GHL("Keep all commits and add a merge commit.")
        case .squash: GHL("Combine all changes into one commit.")
        case .rebase: GHL("Replay each commit onto the target branch.")
        }
    }
}
