import AppKit

@MainActor
public final class GitHubDiffViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    public enum Layout: Equatable, Sendable { case unified, split }

    public private(set) var contentHeight: CGFloat = 0
    public var onContentHeightChange: (@MainActor (CGFloat) -> Void)? {
        didSet { onContentHeightChange?(contentHeight) }
    }
    public var backgroundColor: NSColor = .textBackgroundColor {
        didSet {
            guard isViewLoaded else { return }
            updateBackgroundColor()
        }
    }

    private let scrollView = NSScrollView()
    private let tableView = GitHubDiffTableView()
    private let afterScrollView = NSScrollView()
    private let afterTableView = GitHubDiffTableView()
    private let splitDivider = NSBox()
    private let emptyLabel = NSTextField(labelWithString: "")
    private let highlighter = GitHubCodeHighlighter(fontSize: 13)
    private let changeMap = GitHubDiffChangeMap()
    private var document = GitHubDiffDocument(files: [])
    private var rows: [GitHubDiffDisplayRow] = []
    private var patch: String?
    private var fileName: String?
    private var diffLayout: Layout = .split
    private var showsFileHeaders = true
    private var collapsedFiles: Set<Int> = []
    private var revision: UInt64 = 0
    private var parseTask: Task<Void, Never>?
    private var parseWork: Task<GitHubDiffDocument, Never>?
    private var projectionTask: Task<Void, Never>?
    private var projectionWork: Task<[GitHubDiffDisplayRow], Never>?
    private var projectionRevision: UInt64 = 0
    private var isLoadingDocument = false
    private var maximumColumns = 0
    private var synchronizingScroll = false
    private var synchronizingSelection = false
    private var layingOutContent = false
    private(set) var isParsing = false

    public init() { super.init(nibName: nil, bundle: nil) }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    public override func loadView() {
        let root = GitHubDiffRootView()
        root.backgroundColor = backgroundColor
        root.onLayout = { [weak self] in self?.layoutContent() }
        view = root
        for table in [tableView, afterTableView] {
            table.headerView = nil
            table.rowHeight = 22
            table.intercellSpacing = .zero
            table.backgroundColor = backgroundColor
            table.allowsMultipleSelection = true
            table.columnAutoresizingStyle = .noColumnAutoresizing
            table.style = .plain
            table.dataSource = self
            table.delegate = self
        }
        tableView.setAccessibilityIdentifier("timeline.diff.table")
        afterTableView.setAccessibilityIdentifier("timeline.diff.after.table")
        tableView.onCopy = { [weak self] in
            guard let self else { return }
            copySelectedRows(in: tableView)
        }
        afterTableView.onCopy = { [weak self] in
            guard let self else { return }
            copySelectedRows(in: afterTableView)
        }
        for identifier in ["before", "after"] {
            let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(identifier))
            column.minWidth = 100
            column.resizingMask = []
            tableView.addTableColumn(column)
        }
        tableView.tableColumns[1].isHidden = true
        let afterColumn = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("after"))
        afterColumn.minWidth = 100
        afterColumn.resizingMask = []
        afterTableView.addTableColumn(afterColumn)
        scrollView.documentView = tableView
        afterScrollView.documentView = afterTableView
        for scroll in [scrollView, afterScrollView] {
            scroll.hasVerticalScroller = true
            scroll.hasHorizontalScroller = true
            scroll.autohidesScrollers = true
            scroll.drawsBackground = false
            scroll.borderType = .noBorder
            scroll.contentView.postsBoundsChangedNotifications = true
            NotificationCenter.default.addObserver(self, selector: #selector(scrollBoundsChanged(_:)),
                                                   name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        }
        afterScrollView.isHidden = true
        splitDivider.boxType = .separator
        splitDivider.isHidden = true
        emptyLabel.alignment = .center
        emptyLabel.textColor = .secondaryLabelColor
        emptyLabel.font = .systemFont(ofSize: 13)
        emptyLabel.isSelectable = true
        changeMap.onSelectRow = { [weak self] row in self?.tableView.scrollRowToVisible(row) }
        for child in [scrollView, afterScrollView, splitDivider, emptyLabel, changeMap] { view.addSubview(child) }
    }

    public func configure(patch: String, layout: Layout = .split, showsFileHeaders: Bool = true, fileName: String? = nil) {
        loadViewIfNeeded()
        let contentChanged = self.patch != patch || self.fileName != fileName
        let layoutChanged = diffLayout != layout || self.showsFileHeaders != showsFileHeaders
        guard contentChanged || layoutChanged else { return }
        diffLayout = layout
        self.showsFileHeaders = showsFileHeaders
        if !contentChanged {
            if !isLoadingDocument { reloadRows() }
            return
        }
        self.patch = patch
        self.fileName = fileName
        collapsedFiles.removeAll()
        parseTask?.cancel()
        parseWork?.cancel()
        projectionTask?.cancel()
        projectionWork?.cancel()
        rows.removeAll(keepingCapacity: true)
        updateContentHeight()
        tableView.reloadData()
        afterTableView.reloadData()
        revision &+= 1
        let expected = revision
        isParsing = true
        isLoadingDocument = true
        emptyLabel.stringValue = GHL("Preparing preview…")
        emptyLabel.isHidden = false
        scrollView.isHidden = true
        afterScrollView.isHidden = true
        splitDivider.isHidden = true
        changeMap.isHidden = true
        let work = Task.detached(priority: .userInitiated) { GitHubDiffDocument.parse(patch) }
        parseWork = work
        parseTask = Task { [weak self] in
            let parsed = await work.value
            guard let self, !Task.isCancelled, revision == expected else { return }
            document = parsed
            isLoadingDocument = false
            maximumColumns = parsed.maximumColumns
            reloadRows(resettingScroll: true)
        }
    }

    public func invalidate() {
        revision &+= 1
        parseTask?.cancel()
        parseWork?.cancel()
        projectionTask?.cancel()
        projectionWork?.cancel()
        parseTask = nil
        parseWork = nil
        projectionTask = nil
        projectionWork = nil
        patch = nil
        fileName = nil
        isParsing = false
        isLoadingDocument = false
        rows.removeAll(keepingCapacity: true)
        updateContentHeight()
        if isViewLoaded { applyRows() }
    }

    public func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

    public func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard rows.indices.contains(row) else { return nil }
        let model = rows[row]
        let isRight = tableColumn?.identifier.rawValue == "after"
        if model.kind == .file {
            if isRight { return nil }
            let identifier = NSUserInterfaceItemIdentifier("diff.file")
            let cell = tableView.makeView(withIdentifier: identifier, owner: nil) as? GitHubDiffFileCell ?? GitHubDiffFileCell()
            cell.identifier = identifier
            let file = document.files[model.fileIndex]
            cell.configure(title: file.title, additions: file.additions, deletions: file.deletions, collapsed: collapsedFiles.contains(model.fileIndex))
            cell.onToggle = { [weak self] in self?.toggleFile(model.fileIndex) }
            return cell
        }
        let identifier = NSUserInterfaceItemIdentifier("diff.line")
        let cell = tableView.makeView(withIdentifier: identifier, owner: nil) as? GitHubDiffLineCell ?? GitHubDiffLineCell()
        cell.identifier = identifier
        if model.kind != .line {
            cell.configureMetadata(isRight ? "" : model.title, isHunk: model.kind == .hunk)
            return cell
        }
        let line = isRight ? model.right : model.left
        let changedRange = isRight ? model.rightChange : model.leftChange
        let file = document.files[model.fileIndex]
        let path = isRight ? file.newPath : file.oldPath == "/dev/null" ? file.newPath : file.oldPath
        let language = GitHubSourceDocument.language(fileName: path.isEmpty ? fileName ?? "" : path)
        cell.configure(line: line, split: diffLayout == .split, isRight: isRight, highlighter: highlighter, language: language, changedRange: changedRange)
        return cell
    }

    public func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        guard rows.indices.contains(row) else { return nil }
        let view = GitHubDiffRowView()
        view.row = rows[row]
        view.isRight = tableView === afterTableView
        view.backgroundColor = backgroundColor
        return view
    }

    public func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat { rows[row].kind == .file ? 36 : 22 }

    public func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool { rows[row].kind != .file }

    public func tableViewSelectionDidChange(_ notification: Notification) {
        guard diffLayout == .split, !synchronizingSelection,
              let source = notification.object as? NSTableView,
              source === tableView || source === afterTableView else { return }
        let target = source === tableView ? afterTableView : tableView
        guard target.selectedRowIndexes != source.selectedRowIndexes else { return }
        synchronizingSelection = true
        defer { synchronizingSelection = false }
        target.selectRowIndexes(source.selectedRowIndexes, byExtendingSelection: false)
    }

    @objc private func scrollBoundsChanged(_ notification: Notification) {
        guard diffLayout == .split, !isParsing, !layingOutContent, !synchronizingScroll,
              let source = notification.object as? NSClipView else { return }
        let target = source === scrollView.contentView ? afterScrollView : scrollView
        guard target.contentView.bounds.origin.y != source.bounds.origin.y else { return }
        synchronizingScroll = true
        defer { synchronizingScroll = false }
        let origin = NSPoint(x: target.contentView.bounds.origin.x, y: source.bounds.origin.y)
        target.contentView.scroll(to: origin)
        target.reflectScrolledClipView(target.contentView)
    }

    private func toggleFile(_ index: Int) {
        if !collapsedFiles.insert(index).inserted { collapsedFiles.remove(index) }
        reloadRows()
    }

    private func reloadRows(resettingScroll: Bool = false) {
        projectionTask?.cancel()
        projectionWork?.cancel()
        projectionRevision &+= 1
        let expectedProjection = projectionRevision
        let expectedContent = revision
        let document = document
        let split = diffLayout == .split
        let collapsed = collapsedFiles
        let headers = showsFileHeaders
        let origin = resettingScroll ? NSPoint.zero : scrollView.contentView.bounds.origin
        let afterOrigin = resettingScroll ? NSPoint.zero : afterScrollView.contentView.bounds.origin
        isParsing = true
        emptyLabel.stringValue = GHL("Preparing preview…")
        emptyLabel.isHidden = false
        scrollView.isHidden = true
        afterScrollView.isHidden = true
        splitDivider.isHidden = true
        changeMap.isHidden = true
        let work = Task.detached(priority: .userInitiated) {
            GitHubDiffDisplayRow.rows(document: document, split: split, collapsedFiles: collapsed, showsFileHeaders: headers)
        }
        projectionWork = work
        projectionTask = Task { [weak self] in
            let projected = await work.value
            guard let self, !Task.isCancelled, revision == expectedContent, projectionRevision == expectedProjection else { return }
            rows = projected
            isParsing = false
            applyRows()
            restoreScrollPositions(before: origin, after: afterOrigin)
        }
    }

    private func applyRows() {
        updateContentHeight()
        tableView.reloadData()
        afterTableView.reloadData()
        scrollView.isHidden = rows.isEmpty
        afterScrollView.isHidden = rows.isEmpty || diffLayout != .split
        splitDivider.isHidden = afterScrollView.isHidden
        emptyLabel.isHidden = !rows.isEmpty
        emptyLabel.stringValue = GHL("No diff content is available for this file.")
        changeMap.rows = rows
        changeMap.isHidden = rows.isEmpty || !rows.contains { $0.left?.kind == .deletion || $0.left?.kind == .addition || $0.right?.kind == .addition }
        layoutContent()
    }

    private func updateContentHeight() {
        let height = rows.reduce(CGFloat.zero) { $0 + ($1.kind == .file ? 36 : 22) }
        guard contentHeight != height else { return }
        contentHeight = height
        onContentHeightChange?(height)
    }

    private func updateBackgroundColor() {
        (view as? GitHubDiffRootView)?.backgroundColor = backgroundColor
        for table in [tableView, afterTableView] {
            table.backgroundColor = backgroundColor
            table.enumerateAvailableRowViews { row, _ in
                row.backgroundColor = self.backgroundColor
                row.needsDisplay = true
            }
            table.needsDisplay = true
        }
    }

    private func layoutContent() {
        guard isViewLoaded, !layingOutContent else { return }
        layingOutContent = true
        defer { layingOutContent = false }
        let beforeOrigin = scrollView.contentView.bounds.origin
        let afterOrigin = afterScrollView.contentView.bounds.origin
        let mapWidth: CGFloat = changeMap.isHidden ? 0 : 10
        let availableWidth = max(0, view.bounds.width - mapWidth)
        let split = diffLayout == .split
        let beforeWidth = split ? max(0, floor((availableWidth - 1) / 2)) : availableWidth
        scrollView.hasVerticalScroller = !split
        scrollView.autohidesScrollers = !split
        afterScrollView.autohidesScrollers = false
        scrollView.frame = NSRect(x: 0, y: 0, width: beforeWidth, height: view.bounds.height)
        afterScrollView.frame = NSRect(x: beforeWidth + 1, y: 0, width: max(0, availableWidth - beforeWidth - 1), height: view.bounds.height)
        splitDivider.frame = NSRect(x: beforeWidth, y: 0, width: 1, height: view.bounds.height)
        changeMap.frame = NSRect(x: availableWidth, y: 0, width: mapWidth, height: view.bounds.height)
        emptyLabel.frame = NSRect(x: 16, y: max(16, view.bounds.midY - 20), width: max(0, view.bounds.width - 32), height: 40)
        let column = ("0" as NSString).size(withAttributes: [.font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)]).width
        let minimum = CGFloat(maximumColumns) * column + (split ? 54 : 88)
        let beforeDocumentWidth = max(scrollView.contentSize.width, minimum)
        let afterDocumentWidth = max(afterScrollView.contentSize.width, minimum)
        tableView.tableColumns[0].width = beforeDocumentWidth
        tableView.frame.size.width = beforeDocumentWidth
        afterTableView.tableColumns[0].width = afterDocumentWidth
        afterTableView.frame.size.width = afterDocumentWidth
        restoreScrollPositions(before: beforeOrigin, after: afterOrigin)
    }

    private func restoreScrollPositions(before: NSPoint, after: NSPoint) {
        let wasSynchronizing = synchronizingScroll
        synchronizingScroll = true
        defer { synchronizingScroll = wasSynchronizing }
        scrollView.contentView.scroll(to: before)
        scrollView.reflectScrolledClipView(scrollView.contentView)
        afterScrollView.contentView.scroll(to: NSPoint(x: after.x, y: scrollView.contentView.bounds.origin.y))
        afterScrollView.reflectScrolledClipView(afterScrollView.contentView)
    }

    private func copySelectedRows(in source: NSTableView) {
        let selected = source.selectedRowIndexes.compactMap { index -> String? in
            guard rows.indices.contains(index) else { return nil }
            let row = rows[index]
            if row.kind != .line { return row.title }
            if diffLayout == .split {
                return [row.left?.content, row.right?.content].compactMap { $0 }.joined(separator: "\t")
            }
            return row.left?.content
        }.joined(separator: "\n")
        guard !selected.isEmpty else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(selected, forType: .string)
    }
}

@MainActor
private final class GitHubDiffTableView: NSTableView {
    var onCopy: (() -> Void)?
    @objc func copy(_ sender: Any?) { onCopy?() }
}

@MainActor
private final class GitHubDiffRootView: NSView {
    var onLayout: (() -> Void)?
    var backgroundColor: NSColor = .textBackgroundColor {
        didSet { needsDisplay = true }
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        clipsToBounds = true
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    override var isFlipped: Bool { true }
    override var wantsDefaultClipping: Bool { true }
    override func layout() { super.layout(); onLayout?() }
    override func draw(_ dirtyRect: NSRect) {
        backgroundColor.setFill()
        dirtyRect.intersection(bounds).fill()
    }
}

@MainActor
private final class GitHubDiffLineCell: NSTableCellView {
    private let numbers = NSTextField(labelWithString: "")
    private let content = NSTextField(labelWithString: "")
    private var gutter: CGFloat = 54

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        numbers.font = .monospacedDigitSystemFont(ofSize: 11, weight: .regular)
        numbers.textColor = .secondaryLabelColor
        numbers.alignment = .right
        content.isSelectable = true
        content.lineBreakMode = .byClipping
        content.maximumNumberOfLines = 1
        addSubview(numbers)
        addSubview(content)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func configure(line: GitHubDiffDocument.Line?, split: Bool, isRight: Bool, highlighter: GitHubCodeHighlighter, language: String?, changedRange: NSRange?) {
        gutter = split ? 54 : 88
        guard let line else { numbers.stringValue = ""; content.stringValue = ""; return }
        let marker = line.kind == .addition ? "+" : line.kind == .deletion ? "−" : " "
        if split { numbers.stringValue = "\(isRight ? line.newNumber.map(String.init) ?? "" : line.oldNumber.map(String.init) ?? "")  \(marker)" }
        else { numbers.stringValue = "\(line.oldNumber.map(String.init) ?? "")  \(line.newNumber.map(String.init) ?? "")  \(marker)" }
        let text = NSMutableAttributedString(attributedString: highlighter.attributedString(code: line.content, language: language))
        let style = NSMutableParagraphStyle()
        style.lineBreakMode = .byClipping
        style.tabStops = []
        style.defaultTabInterval = ("    " as NSString).size(withAttributes: [.font: NSFont.monospacedSystemFont(ofSize: 13, weight: .regular)]).width
        text.addAttribute(.paragraphStyle, value: style, range: NSRange(location: 0, length: text.length))
        if let changedRange, changedRange.length > 0, NSMaxRange(changedRange) <= text.length {
            let color = line.kind == .addition ? NSColor.systemGreen : NSColor.systemRed
            text.addAttribute(.backgroundColor, value: color.withAlphaComponent(0.18), range: changedRange)
        }
        content.attributedStringValue = text
        setAccessibilityLabel(numbers.stringValue + " " + line.content)
        needsLayout = true
    }

    func configureMetadata(_ text: String, isHunk: Bool) {
        gutter = 14
        numbers.stringValue = ""
        content.attributedStringValue = NSAttributedString(string: text, attributes: [.font: NSFont.monospacedSystemFont(ofSize: 11, weight: .regular), .foregroundColor: isHunk ? NSColor.secondaryLabelColor : NSColor.tertiaryLabelColor])
        needsLayout = true
    }

    override func layout() {
        super.layout()
        numbers.frame = NSRect(x: 4, y: max(0, (bounds.height - 16) / 2), width: max(0, gutter - 12), height: 16)
        content.frame = NSRect(x: gutter, y: max(0, (bounds.height - 18) / 2), width: max(0, bounds.width - gutter - 12), height: 18)
    }
}

@MainActor
private final class GitHubDiffFileCell: NSTableCellView {
    private let button = NSButton(title: "", target: nil, action: nil)
    var onToggle: (() -> Void)?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        button.isBordered = false
        button.alignment = .left
        button.font = .systemFont(ofSize: 12, weight: .medium)
        button.lineBreakMode = .byTruncatingMiddle
        button.target = self
        button.action = #selector(toggle)
        addSubview(button)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    func configure(title: String, additions: Int, deletions: Int, collapsed: Bool) {
        button.title = title + "   +\(additions) −\(deletions)"
        button.image = NSImage(systemSymbolName: collapsed ? "chevron.right" : "chevron.down", accessibilityDescription: nil)
        button.imagePosition = .imageLeading
        button.setAccessibilityLabel(title)
        button.setAccessibilityValue(collapsed ? GHL("Collapsed") : GHL("Expanded"))
    }

    override func layout() { super.layout(); button.frame = bounds.insetBy(dx: 12, dy: 4) }
    @objc private func toggle() { onToggle?() }
}

@MainActor
private final class GitHubDiffRowView: NSTableRowView {
    var row: GitHubDiffDisplayRow?
    var isRight = false

    override func drawBackground(in dirtyRect: NSRect) {
        backgroundColor.setFill()
        dirtyRect.fill()
        guard let row else { return }
        if row.kind == .file || row.kind == .hunk {
            NSColor.labelColor.withAlphaComponent(row.kind == .file ? 0.04 : 0.025).setFill()
            dirtyRect.fill()
            return
        }
        func fill(_ line: GitHubDiffDocument.Line?, rect: NSRect) {
            guard let kind = line?.kind, kind == .addition || kind == .deletion else { return }
            let color = kind == .addition ? NSColor.systemGreen : NSColor.systemRed
            color.withAlphaComponent(0.09).setFill()
            rect.intersection(dirtyRect).fill()
        }
        fill(isRight ? row.right : row.left, rect: bounds)
    }
}

@MainActor
private final class GitHubDiffChangeMap: NSView {
    private var changes: [(row: Int, mask: Int)] = []
    private var cachedHeight: CGFloat = -1
    private var bins: [Int] = []
    var rows: [GitHubDiffDisplayRow] = [] {
        didSet {
            changes = rows.enumerated().compactMap { index, row in
                let mask = (row.left?.kind == .deletion ? 1 : 0)
                    | (row.right?.kind == .addition || row.left?.kind == .addition ? 2 : 0)
                return mask == 0 ? nil : (index, mask)
            }
            cachedHeight = -1
            needsDisplay = true
        }
    }
    var onSelectRow: ((Int) -> Void)?
    override var isFlipped: Bool { true }

    override func draw(_ dirtyRect: NSRect) {
        guard !rows.isEmpty, bounds.height > 0 else { return }
        if cachedHeight != bounds.height {
            cachedHeight = bounds.height
            bins = Array(repeating: 0, count: max(1, Int(ceil(bounds.height))))
            for change in changes {
                let index = min(bins.count - 1, change.row * bins.count / rows.count)
                bins[index] |= change.mask
            }
        }
        for (index, mask) in bins.enumerated() where mask != 0 {
            let color = mask == 3 ? NSColor.systemOrange : mask == 2 ? NSColor.systemGreen : NSColor.systemRed
            color.withAlphaComponent(0.65).setFill()
            NSRect(x: 2, y: CGFloat(index), width: max(0, bounds.width - 4), height: 2).fill()
        }
    }

    override func mouseDown(with event: NSEvent) {
        guard !rows.isEmpty, bounds.height > 0 else { return }
        let point = convert(event.locationInWindow, from: nil)
        let row = min(rows.count - 1, max(0, Int(point.y / bounds.height * CGFloat(rows.count))))
        onSelectRow?(row)
    }
}
