import GitHubCore
import SwiftUI

struct GitHubChecksView: View {
    @Bindable var service: GitHubService
    private var checks: [GitHubValue] { service.sections[.checks]?.items ?? [] }
    private var workflows: [GitHubValue] { service.sections[.workflows]?.items ?? [] }
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 14) {
                if checks.isEmpty, workflows.isEmpty, service.sections[.checks]?.loading == true { GitHubSkeleton(rows: 4) }
                ForEach(workflows, id: \.id) { run in GitHubWorkflowView(service: service, run: run) }
                if !checks.isEmpty {
                    Text(GHL("Checks")).font(.system(size: 13, weight: .semibold)).padding(.top, 4)
                    VStack(spacing: 0) {
                        ForEach(checks, id: \.id) { check in
                            GitHubCheckRow(value: check)
                            if check.id != checks.last?.id { Divider() }
                        }
                    }.githubOutline()
                }
                let statuses = service.sections[.statuses]?.items ?? []
                if !statuses.isEmpty {
                    Text(GHL("Commit statuses")).font(.system(size: 13, weight: .semibold))
                    VStack(spacing: 0) {
                        ForEach(statuses, id: \.id) { value in
                            HStack(spacing: 10) {
                                GitHubCheckState(value: value)
                                VStack(alignment: .leading, spacing: 4) {
                                    Text(value["context"].text).fontWeight(.medium)
                                    Text(value["description"].text).font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                GitHubDate(date: value["updated_at"].date)
                            }.padding(12)
                            if value.id != statuses.last?.id { Divider() }
                        }
                    }.githubOutline()
                }
                if checks.isEmpty, workflows.isEmpty, statuses.isEmpty, service.sections[.checks]?.loaded == true {
                    Text(GHL("No checks for this commit.")).foregroundStyle(.secondary).frame(maxWidth: .infinity).padding(32)
                }
                ForEach([GitHubSection.checks, .statuses, .workflows], id: \.self) { GitHubSectionError(service: service, section: $0) }
            }.padding(24)
        }
    }
}

private struct GitHubCheckRow: View {
    let value: GitHubValue
    var body: some View {
        DisclosureGroup {
            VStack(alignment: .leading, spacing: 12) {
                if !value["output"]["title"].text.isEmpty { Text(value["output"]["title"].text).fontWeight(.medium) }
                if !value["output"]["summary"].text.isEmpty { GitHubMarkdownView(source: value["output"]["summary"].text) }
                if !value["output"]["text"].text.isEmpty { GitHubMarkdownView(source: value["output"]["text"].text) }
                if value["output"]["summary"].text.isEmpty, value["output"]["text"].text.isEmpty {
                    Text(value["conclusion"].string ?? value["status"].text).foregroundStyle(.secondary)
                }
            }.padding(.top, 12).padding(.leading, 23)
        } label: {
            HStack(spacing: 9) {
                GitHubCheckState(value: value)
                Text(value["name"].text).font(.system(size: 12, weight: .medium)).lineLimit(2)
                Spacer()
                Text(value["app"]["name"].text).font(.caption).foregroundStyle(.secondary)
            }
        }.padding(12)
    }
}

private struct GitHubWorkflowView: View {
    @Bindable var service: GitHubService
    let run: GitHubValue
    @State private var expanded = false
    @State private var jobs: [GitHubValue] = []
    @State private var loading = false
    @State private var error: String?
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 10) {
                Button { expanded.toggle() } label: {
                    HStack(spacing: 9) {
                        Image(systemName: expanded ? "chevron.down" : "chevron.right").font(.system(size: 9)).foregroundStyle(.secondary)
                        GitHubCheckState(value: run)
                        VStack(alignment: .leading, spacing: 3) {
                            Text(run["name"].text).font(.system(size: 12, weight: .semibold))
                            Text(run["display_title"].text).font(.system(size: 11)).foregroundStyle(.secondary).lineLimit(1)
                        }
                    }.contentShape(Rectangle())
                }.buttonStyle(.plain)
                Spacer()
                GitHubDate(date: run["updated_at"].date, relative: true)
                if service.sections[.repository]?.data["permissions"]["push"].bool == true {
                    if run["status"].text == "completed" {
                        Button(GHL("Re-run")) { Task { await service.rerunWorkflow(id: run["id"].int ?? 0) } }
                            .buttonStyle(.bordered).controlSize(.small)
                    } else {
                        Button(GHL("Cancel run")) { Task { await service.cancelWorkflow(id: run["id"].int ?? 0) } }
                            .buttonStyle(.bordered).controlSize(.small)
                    }
                }
            }.padding(14)
            if expanded {
                Divider()
                VStack(alignment: .leading, spacing: 8) {
                    if loading { GitHubSkeleton(rows: 2, sidebar: true) }
                    ForEach(jobs, id: \.id) { job in
                        DisclosureGroup {
                            ForEach(job["steps"].array, id: \.self) { step in
                                HStack(spacing: 8) {
                                    GitHubCheckState(value: step)
                                    Text(step["name"].text).font(.system(size: 11))
                                    Spacer()
                                    Text(step["conclusion"].string ?? step["status"].text).font(.system(size: 10)).foregroundStyle(.secondary)
                                }.padding(.vertical, 4).padding(.leading, 22)
                            }
                        } label: {
                            HStack(spacing: 8) {
                                GitHubCheckState(value: job)
                                Text(job["name"].text).font(.system(size: 12))
                            }
                        }.padding(.vertical, 4)
                    }
                    if let error { Text(error).font(.caption).foregroundStyle(.red).textSelection(.enabled) }
                }.padding(14)
            }
        }
        .githubOutline()
        .task(id: expanded) {
            guard expanded else { return }
            loading = true; error = nil
            do { jobs = try await service.workflowJobs(id: run["id"].int ?? 0) }
            catch { self.error = error.localizedDescription }
            loading = false
        }
    }
}

private struct GitHubCheckState: View {
    let value: GitHubValue
    private var state: String { value["conclusion"].string ?? value["state"].string ?? value["status"].text }
    var body: some View {
        Group {
            switch state {
            case "success", "neutral", "skipped": Image(systemName: "checkmark.circle").foregroundStyle(state == "success" ? .green : .secondary)
            case "failure", "error", "timed_out", "action_required", "startup_failure": Image(systemName: "xmark.circle").foregroundStyle(.red)
            case "cancelled", "stale": Image(systemName: "minus.circle").foregroundStyle(.secondary)
            case "in_progress": ProgressView().controlSize(.mini)
            default: Image(systemName: "clock").foregroundStyle(.orange)
            }
        }.frame(width: 15, height: 15).accessibilityLabel(state)
    }
}

private extension View {
    func githubOutline() -> some View {
        background(Color.primary.opacity(0.012), in: RoundedRectangle(cornerRadius: 6))
            .overlay { RoundedRectangle(cornerRadius: 6).strokeBorder(Color.primary.opacity(0.09)) }
    }
}
