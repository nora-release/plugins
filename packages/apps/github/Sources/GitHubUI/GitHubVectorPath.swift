//
//  GitHubVectorPath.swift
//  Nora
//
//  A small SVG path-data renderer. Parses the `d` attribute of an SVG <path>
//  (the subset used by our HugeIcons assets: M/m L/l H/h V/v C/c S/s Q/q T/t
//  A/a Z/z) and produces a SwiftUI `Path`, uniformly scaled and centered into
//  a target rect from the icon's viewBox.
//
//  Icons are defined by their raw SVG path string so geometry comes straight
//  from the authoritative asset — no hand-transcribed control points.
//

import CoreGraphics
import SwiftUI

enum GitHubVectorPath {
    /// Build a `Path` from SVG path data, scaled to fit `rect` while preserving
    /// aspect ratio and centered, using the given `viewBox` size (default 24x24).
    nonisolated static func path(
        _ data: String,
        in rect: CGRect,
        viewBox: CGSize = CGSize(width: 24, height: 24)
    ) -> Path {
        var builder = Builder()
        let tokens = tokenize(data)
        builder.run(tokens)

        let scale = min(rect.width / viewBox.width, rect.height / viewBox.height)
        let tx = rect.midX - viewBox.width * scale / 2
        let ty = rect.midY - viewBox.height * scale / 2
        let transform = CGAffineTransform(a: scale, b: 0, c: 0, d: scale, tx: tx, ty: ty)
        return builder.path.applying(transform)
    }

    /// Combine multiple SVG path data strings into a single `Path`,
    /// uniformly scaled and centered into the target rect.
    nonisolated static func combinedPath(
        _ dataList: [String],
        in rect: CGRect,
        viewBox: CGSize = CGSize(width: 24, height: 24)
    ) -> Path {
        var combined = Path()
        for data in dataList {
            combined.addPath(path(data, in: rect, viewBox: viewBox))
        }
        return combined
    }

    // MARK: - Tokenizer

    enum Token {
        case command(Character)
        case number(CGFloat)
    }

    /// Split path data into a flat stream of command letters and numbers.
    /// Handles packed numbers ("6.00081"), comma/space separators, leading
    /// signs, decimals without a leading zero (".21"), exponents, and the
    /// "second decimal point begins a new number" rule (e.g. "1.5.5").
    nonisolated static func tokenize(_ data: String) -> [Token] {
        var tokens: [Token] = []
        let chars = Array(data)
        var i = 0
        let n = chars.count

        while i < n {
            let c = chars[i]
            if c.isLetter {
                tokens.append(.command(c))
                i += 1
            } else if c == " " || c == "," || c == "\n" || c == "\t" || c == "\r" {
                i += 1
            } else {
                // Parse a number starting at i.
                var j = i
                var seenDot = false
                var seenExp = false
                if j < n, chars[j] == "+" || chars[j] == "-" {
                    j += 1
                }
                while j < n {
                    let ch = chars[j]
                    if ch.isNumber {
                        j += 1
                    } else if ch == "." {
                        if seenDot || seenExp {
                            break
                        }
                        seenDot = true
                        j += 1
                    } else if ch == "e" || ch == "E" {
                        if seenExp {
                            break
                        }
                        seenExp = true
                        j += 1
                        if j < n, chars[j] == "+" || chars[j] == "-" {
                            j += 1
                        }
                    } else {
                        break
                    }
                }
                let slice = String(chars[i ..< j])
                if let value = Double(slice) {
                    tokens.append(.number(CGFloat(value)))
                }
                i = max(j, i + 1)
            }
        }
        return tokens
    }

    // MARK: - Builder

    /// Consumes a token stream and emits a `Path` in viewBox coordinates.
    struct Builder {
        var path = Path()
        private var current = CGPoint.zero
        private var start = CGPoint.zero
        /// Reflection control point from the previous C/S/Q/T command, for smooth curves.
        private var lastControl: CGPoint?
        private var lastCommand: Character = " "

        nonisolated init() {}

        nonisolated mutating func run(_ tokens: [GitHubVectorPath.Token]) {
            var idx = 0
            let count = tokens.count

            func nextNumber() -> CGFloat? {
                while idx < count {
                    if case let .number(value) = tokens[idx] {
                        idx += 1
                        return value
                    }
                    idx += 1
                }
                return nil
            }

            while idx < count {
                guard case let .command(cmd) = tokens[idx] else {
                    // Implicit repeat: reuse previous command on a bare number run.
                    if lastCommand != " " {
                        consume(lastCommand, nextNumber)
                        continue
                    }
                    idx += 1
                    continue
                }
                idx += 1
                consume(cmd, nextNumber)
            }
        }

        // MARK: Command dispatch

        private nonisolated mutating func consume(
            _ cmd: Character,
            _ next: () -> CGFloat?
        ) {
            let relative = cmd.isLowercase
            switch Character(cmd.lowercased()) {
            case "m":
                guard let x = next(), let y = next() else { return }
                let p = resolve(x, y, relative)
                path.move(to: p)
                current = p
                start = p
                // Subsequent implicit pairs after a moveto are linetos.
                lastCommand = relative ? "l" : "L"
                lastControl = nil
                return
            case "l":
                guard let x = next(), let y = next() else { return }
                let p = resolve(x, y, relative)
                path.addLine(to: p)
                current = p
            case "h":
                guard let x = next() else { return }
                let p = CGPoint(x: relative ? current.x + x : x, y: current.y)
                path.addLine(to: p)
                current = p
            case "v":
                guard let y = next() else { return }
                let p = CGPoint(x: current.x, y: relative ? current.y + y : y)
                path.addLine(to: p)
                current = p
            case "c":
                guard let x1 = next(), let y1 = next(),
                      let x2 = next(), let y2 = next(),
                      let x = next(), let y = next() else { return }
                let c1 = resolve(x1, y1, relative)
                let c2 = resolve(x2, y2, relative)
                let p = resolve(x, y, relative)
                path.addCurve(to: p, control1: c1, control2: c2)
                lastControl = c2
                current = p
                lastCommand = cmd
                return
            case "s":
                guard let x2 = next(), let y2 = next(),
                      let x = next(), let y = next() else { return }
                let c1 = reflectedControl()
                let c2 = resolve(x2, y2, relative)
                let p = resolve(x, y, relative)
                path.addCurve(to: p, control1: c1, control2: c2)
                lastControl = c2
                current = p
                lastCommand = cmd
                return
            case "q":
                guard let x1 = next(), let y1 = next(),
                      let x = next(), let y = next() else { return }
                let c = resolve(x1, y1, relative)
                let p = resolve(x, y, relative)
                path.addQuadCurve(to: p, control: c)
                lastControl = c
                current = p
                lastCommand = cmd
                return
            case "t":
                guard let x = next(), let y = next() else { return }
                let c = reflectedControl()
                let p = resolve(x, y, relative)
                path.addQuadCurve(to: p, control: c)
                lastControl = c
                current = p
                lastCommand = cmd
                return
            case "a":
                guard let rx = next(), let ry = next(),
                      let rot = next(), let large = next(),
                      let sweep = next(), let x = next(), let y = next() else { return }
                let p = resolve(x, y, relative)
                addArc(rx: rx, ry: ry, rotationDeg: rot,
                       largeArc: large != 0, sweep: sweep != 0, to: p)
                current = p
            case "z":
                path.closeSubpath()
                current = start
            default:
                return
            }
            lastControl = nil
            lastCommand = cmd
        }

        // MARK: Helpers

        /// Resolve a coordinate pair to absolute viewBox space.
        private nonisolated func resolve(_ x: CGFloat, _ y: CGFloat, _ relative: Bool) -> CGPoint {
            relative ? CGPoint(x: current.x + x, y: current.y + y) : CGPoint(x: x, y: y)
        }

        /// Reflection of the previous control point about the current point,
        /// for smooth-curve commands (S/T). Falls back to current point.
        private nonisolated func reflectedControl() -> CGPoint {
            guard let last = lastControl else { return current }
            return CGPoint(x: 2 * current.x - last.x, y: 2 * current.y - last.y)
        }

        /// Append an SVG elliptical arc (endpoint parameterization) to the path,
        /// converting to center parameterization per the SVG 1.1 implementation notes.
        private nonisolated mutating func addArc(
            rx rxIn: CGFloat,
            ry ryIn: CGFloat,
            rotationDeg: CGFloat,
            largeArc: Bool,
            sweep: Bool,
            to end: CGPoint
        ) {
            let p0 = current
            if rxIn == 0 || ryIn == 0 {
                path.addLine(to: end)
                return
            }
            var rx = abs(rxIn)
            var ry = abs(ryIn)
            let phi = rotationDeg * .pi / 180
            let cosPhi = cos(phi)
            let sinPhi = sin(phi)

            // Step 1: compute (x1', y1')
            let dx = (p0.x - end.x) / 2
            let dy = (p0.y - end.y) / 2
            let x1p = cosPhi * dx + sinPhi * dy
            let y1p = -sinPhi * dx + cosPhi * dy

            // Correct out-of-range radii.
            let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry)
            if lambda > 1 {
                let s = sqrt(lambda)
                rx *= s
                ry *= s
            }

            // Step 2: compute center (cx', cy')
            let rxSq = rx * rx
            let rySq = ry * ry
            let x1pSq = x1p * x1p
            let y1pSq = y1p * y1p
            var num = rxSq * rySq - rxSq * y1pSq - rySq * x1pSq
            let den = rxSq * y1pSq + rySq * x1pSq
            if num < 0 {
                num = 0
            }
            var coef = den == 0 ? 0 : sqrt(num / den)
            if largeArc == sweep {
                coef = -coef
            }
            let cxp = coef * (rx * y1p / ry)
            let cyp = coef * -(ry * x1p / rx)

            // Step 3: compute center (cx, cy)
            let cx = cosPhi * cxp - sinPhi * cyp + (p0.x + end.x) / 2
            let cy = sinPhi * cxp + cosPhi * cyp + (p0.y + end.y) / 2

            /// Step 4: compute start angle and sweep angle.
            func angle(_ ux: CGFloat, _ uy: CGFloat, _ vx: CGFloat, _ vy: CGFloat) -> CGFloat {
                let dot = ux * vx + uy * vy
                let len = sqrt((ux * ux + uy * uy) * (vx * vx + vy * vy))
                var a = acos(max(-1, min(1, len == 0 ? 1 : dot / len)))
                if ux * vy - uy * vx < 0 {
                    a = -a
                }
                return a
            }
            let theta1 = angle(1, 0, (x1p - cxp) / rx, (y1p - cyp) / ry)
            var dTheta = angle((x1p - cxp) / rx, (y1p - cyp) / ry,
                               (-x1p - cxp) / rx, (-y1p - cyp) / ry)
            if !sweep, dTheta > 0 {
                dTheta -= 2 * .pi
            }
            if sweep, dTheta < 0 {
                dTheta += 2 * .pi
            }

            // Step 5: approximate the arc with cubic Beziers (<= 90deg segments).
            let segments = max(1, Int(ceil(abs(dTheta) / (.pi / 2))))
            let delta = dTheta / CGFloat(segments)
            let t = 4.0 / 3.0 * tan(delta / 4)

            var angleStart = theta1
            for _ in 0 ..< segments {
                let angleEnd = angleStart + delta
                let cosS = cos(angleStart), sinS = sin(angleStart)
                let cosE = cos(angleEnd), sinE = sin(angleEnd)

                /// Points on the unit circle mapped through the ellipse + rotation.
                func map(_ cosA: CGFloat, _ sinA: CGFloat) -> CGPoint {
                    let ex = rx * cosA
                    let ey = ry * sinA
                    return CGPoint(
                        x: cosPhi * ex - sinPhi * ey + cx,
                        y: sinPhi * ex + cosPhi * ey + cy
                    )
                }
                func deriv(_ cosA: CGFloat, _ sinA: CGFloat) -> CGPoint {
                    let ex = -rx * sinA
                    let ey = ry * cosA
                    return CGPoint(
                        x: cosPhi * ex - sinPhi * ey,
                        y: sinPhi * ex + cosPhi * ey
                    )
                }

                let p1 = map(cosS, sinS)
                let p2 = map(cosE, sinE)
                let d1 = deriv(cosS, sinS)
                let d2 = deriv(cosE, sinE)
                let c1 = CGPoint(x: p1.x + t * d1.x, y: p1.y + t * d1.y)
                let c2 = CGPoint(x: p2.x - t * d2.x, y: p2.y - t * d2.y)
                path.addCurve(to: p2, control1: c1, control2: c2)
                angleStart = angleEnd
            }
        }
    }
}
