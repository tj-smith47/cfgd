use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Html;
use axum::routing::get;

use cfgd_core::xml_escape;

use super::api::{SharedState, extract_bearer_token};
use super::errors::GatewayError;
use super::fleet;

const COMMON_STYLES: &str = r#"
        * { margin: 0; padding: 0; box-sizing: border-box; }
        body { font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
               background: #0f1117; color: #c9d1d9; line-height: 1.6; }
        .container { max-width: 1200px; margin: 0 auto; padding: 2rem; }
        a { color: #58a6ff; text-decoration: none; }
        a:hover { text-decoration: underline; }
        h1 { color: #58a6ff; margin-bottom: 0.5rem; font-size: 1.5rem; }
        .subtitle { color: #8b949e; margin-bottom: 2rem; }
        .nav { display: flex; gap: 1.5rem; margin-bottom: 2rem; padding-bottom: 1rem;
               border-bottom: 1px solid #30363d; font-size: 0.875rem; }
        .nav a { color: #8b949e; }
        .nav a:hover, .nav a.active { color: #58a6ff; text-decoration: none; }
        table { width: 100%; border-collapse: collapse; background: #161b22;
                border: 1px solid #30363d; border-radius: 8px; overflow: hidden; }
        th { text-align: left; padding: 0.75rem 1rem; background: #1c2128; color: #8b949e;
             font-size: 0.75rem; text-transform: uppercase; letter-spacing: 0.05em;
             border-bottom: 1px solid #30363d; }
        td { padding: 0.75rem 1rem; border-bottom: 1px solid #21262d; font-size: 0.875rem; }
        tr:last-child td { border-bottom: none; }
        code { background: #1c2128; padding: 0.15rem 0.4rem; border-radius: 4px;
               font-size: 0.8rem; }
        .status { display: inline-block; padding: 0.15rem 0.5rem; border-radius: 12px;
                   font-size: 0.75rem; font-weight: 600; text-transform: uppercase; }
        .status.healthy { background: #0d3117; color: #3fb950; }
        .status.drifted { background: #3d2e00; color: #d29922; }
        .status.offline { background: #3d1418; color: #f85149; }
        .status.pending-reconcile { background: #1c2541; color: #58a6ff; }
        .muted { color: #8b949e; }
        .empty { text-align: center; padding: 3rem; color: #8b949e; }
"#;

/// Web UI auth: checks Authorization header, session cookie, or ?token= query param.
/// When CFGD_API_KEY is not set, all requests are allowed.
/// If ?token= is used, a session cookie is set and the client is redirected to strip
/// the token from the URL (prevents token leaking into browser history/logs).
async fn web_auth_middleware(
    headers: HeaderMap,
    query: Query<std::collections::HashMap<String, String>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, GatewayError> {
    if let Ok(expected_key) = std::env::var("CFGD_API_KEY") {
        // 1. Authorization header
        if let Some(token) = extract_bearer_token(&headers)
            && token == expected_key
        {
            return Ok(next.run(request).await);
        }

        // 2. Session cookie (set after initial ?token= auth)
        if let Some(cookie_header) = headers.get(axum::http::header::COOKIE)
            && let Ok(cookies) = cookie_header.to_str()
        {
            for cookie in cookies.split(';') {
                if let Some(value) = cookie.trim().strip_prefix("cfgd_session=")
                    && value == expected_key
                {
                    return Ok(next.run(request).await);
                }
            }
        }

        // 3. ?token= query param — validate, set cookie, redirect to strip token from URL
        if let Some(token) = query.get("token")
            && *token == expected_key
        {
            let path = request.uri().path().to_string();
            let response = axum::response::Response::builder()
                .status(axum::http::StatusCode::SEE_OTHER)
                .header(axum::http::header::LOCATION, &path)
                .header(
                    axum::http::header::SET_COOKIE,
                    format!(
                        "cfgd_session={}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400",
                        token
                    ),
                )
                .body(axum::body::Body::empty())
                .map_err(|_| GatewayError::Internal("failed to build redirect response".into()))?;
            return Ok(response);
        }

        return Err(GatewayError::Unauthorized);
    }
    Ok(next.run(request).await)
}

pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/", get(dashboard))
        .route("/devices/{id}", get(device_detail))
        .route("/events", get(fleet_events))
        .route_layer(axum::middleware::from_fn(web_auth_middleware))
}

async fn dashboard(State(state): State<SharedState>) -> Result<Html<String>, GatewayError> {
    let db = state.db.lock().await;
    let status = fleet::get_fleet_status(&db)?;
    let devices = db.list_devices()?;

    let mut device_rows = String::new();
    for d in &devices {
        let status_class = match d.status {
            super::db::DeviceStatus::Healthy => "healthy",
            super::db::DeviceStatus::Drifted => "drifted",
            super::db::DeviceStatus::PendingReconcile => "pending-reconcile",
            super::db::DeviceStatus::Offline => "offline",
        };
        device_rows.push_str(&format!(
            r#"<tr class="clickable" onclick="window.location='/devices/{id_raw}'">
                <td>{id}</td>
                <td>{hostname}</td>
                <td>{os}</td>
                <td>{arch}</td>
                <td><span class="status {status_class}">{status}</span></td>
                <td>{last_checkin}</td>
                <td><code>{hash}</code></td>
            </tr>"#,
            id_raw = xml_escape(&d.id),
            id = xml_escape(&d.id),
            hostname = xml_escape(&d.hostname),
            os = xml_escape(&d.os),
            arch = xml_escape(&d.arch),
            status_class = status_class,
            status = xml_escape(d.status.as_str()),
            last_checkin = xml_escape(&d.last_checkin),
            hash = xml_escape(&d.config_hash),
        ));
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>cfgd - Fleet Dashboard</title>
    <style>
        {styles}
        .stats {{ display: grid; grid-template-columns: repeat(4, 1fr); gap: 1rem; margin-bottom: 2rem; }}
        .stat-card {{ background: #161b22; border: 1px solid #30363d; border-radius: 8px;
                      padding: 1.25rem; text-align: center; }}
        .stat-card .value {{ font-size: 2rem; font-weight: 700; }}
        .stat-card .label {{ color: #8b949e; font-size: 0.875rem; text-transform: uppercase; }}
        .stat-card.total .value {{ color: #58a6ff; }}
        .stat-card.healthy .value {{ color: #3fb950; }}
        .stat-card.drifted .value {{ color: #d29922; }}
        .stat-card.offline .value {{ color: #f85149; }}
        tr.clickable {{ cursor: pointer; }}
        tr.clickable:hover td {{ background: #1c2128; }}
    </style>
</head>
<body>
    <div class="container">
        <h1>cfgd Fleet Dashboard</h1>
        <p class="subtitle">Configuration management control plane</p>
        <div class="nav">
            <a href="/" class="active">Devices</a>
            <a href="/events">Events</a>
        </div>
        <div class="stats">
            <div class="stat-card total">
                <div class="value">{total}</div>
                <div class="label">Total Devices</div>
            </div>
            <div class="stat-card healthy">
                <div class="value">{healthy}</div>
                <div class="label">Healthy</div>
            </div>
            <div class="stat-card drifted">
                <div class="value">{drifted}</div>
                <div class="label">Drifted</div>
            </div>
            <div class="stat-card offline">
                <div class="value">{offline}</div>
                <div class="label">Offline</div>
            </div>
        </div>
        {device_table}
    </div>
</body>
</html>"#,
        styles = COMMON_STYLES,
        total = status.total_devices,
        healthy = status.healthy,
        drifted = status.drifted,
        offline = status.offline,
        device_table = if devices.is_empty() {
            r#"<div class="empty">No devices registered yet. Devices will appear here after their first check-in.</div>"#.to_string()
        } else {
            format!(
                r#"<table>
            <thead>
                <tr>
                    <th>ID</th>
                    <th>Hostname</th>
                    <th>OS</th>
                    <th>Arch</th>
                    <th>Status</th>
                    <th>Last Check-in</th>
                    <th>Config Hash</th>
                </tr>
            </thead>
            <tbody>
                {device_rows}
            </tbody>
        </table>"#
            )
        },
    );

    Ok(Html(html))
}

async fn device_detail(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Html<String>, GatewayError> {
    let db = state.db.lock().await;
    let device = db.get_device(&id)?;
    let drift_events = db.list_drift_events(&id)?;
    let checkin_events = db.list_checkin_events(&id)?;

    let status_class = status_to_class(device.status.as_str());

    let desired_config_html = if let Some(ref config) = device.desired_config {
        let formatted = serde_json::to_string_pretty(config).unwrap_or_default();
        format!(
            r#"<div class="section">
                <h2>Desired Configuration</h2>
                <pre><code>{}</code></pre>
            </div>"#,
            xml_escape(&formatted)
        )
    } else {
        r#"<div class="section">
            <h2>Desired Configuration</h2>
            <p class="muted">No desired config set. Push configuration using the form below or the API.</p>
        </div>"#
            .to_string()
    };

    let mut drift_rows = String::new();
    for e in &drift_events {
        let details_parsed: Vec<serde_json::Value> =
            serde_json::from_str(&e.details).unwrap_or_default();
        let detail_summary: Vec<String> = details_parsed
            .iter()
            .map(|d| {
                let field = d.get("field").and_then(|v| v.as_str()).unwrap_or("?");
                let expected = d.get("expected").and_then(|v| v.as_str()).unwrap_or("?");
                let actual = d.get("actual").and_then(|v| v.as_str()).unwrap_or("?");
                format!(
                    "{}: {} &rarr; {}",
                    xml_escape(field),
                    xml_escape(expected),
                    xml_escape(actual)
                )
            })
            .collect();
        drift_rows.push_str(&format!(
            r#"<tr>
                <td>{timestamp}</td>
                <td>{id}</td>
                <td>{details}</td>
            </tr>"#,
            timestamp = xml_escape(&e.timestamp),
            id = xml_escape(&e.id),
            details = if detail_summary.is_empty() {
                xml_escape(&e.details)
            } else {
                detail_summary.join("<br>")
            },
        ));
    }

    let drift_html = if drift_events.is_empty() {
        r#"<p class="muted">No drift events recorded for this device.</p>"#.to_string()
    } else {
        format!(
            r#"<table>
            <thead>
                <tr>
                    <th>Timestamp</th>
                    <th>Event ID</th>
                    <th>Details</th>
                </tr>
            </thead>
            <tbody>
                {drift_rows}
            </tbody>
        </table>"#
        )
    };

    let mut checkin_rows = String::new();
    for c in &checkin_events {
        let changed_badge = if c.config_changed {
            r#"<span class="status drifted">changed</span>"#
        } else {
            r#"<span class="status healthy">ok</span>"#
        };
        checkin_rows.push_str(&format!(
            r#"<tr>
                <td>{timestamp}</td>
                <td><code>{config_hash}</code></td>
                <td>{changed}</td>
            </tr>"#,
            timestamp = xml_escape(&c.timestamp),
            config_hash = xml_escape(&c.config_hash),
            changed = changed_badge,
        ));
    }

    let checkin_html = if checkin_events.is_empty() {
        r#"<p class="muted">No check-in events recorded for this device.</p>"#.to_string()
    } else {
        format!(
            r#"<table>
            <thead>
                <tr>
                    <th>Timestamp</th>
                    <th>Config Hash</th>
                    <th>Config Changed</th>
                </tr>
            </thead>
            <tbody>
                {checkin_rows}
            </tbody>
        </table>"#
        )
    };

    let device_id_js = xml_escape(&device.id);

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>cfgd - {hostname}</title>
    <style>
        {styles}
        .breadcrumb {{ color: #8b949e; margin-bottom: 1.5rem; font-size: 0.875rem; }}
        h1 {{ margin-bottom: 0.25rem; }}
        .meta {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr));
                 gap: 1rem; margin-bottom: 2rem; }}
        .meta-item {{ background: #161b22; border: 1px solid #30363d; border-radius: 8px;
                      padding: 1rem; }}
        .meta-item .label {{ color: #8b949e; font-size: 0.75rem; text-transform: uppercase;
                             letter-spacing: 0.05em; }}
        .meta-item .value {{ font-size: 1.1rem; margin-top: 0.25rem; }}
        .section {{ margin-bottom: 2rem; }}
        .section h2 {{ color: #c9d1d9; font-size: 1.1rem; margin-bottom: 1rem;
                       padding-bottom: 0.5rem; border-bottom: 1px solid #30363d; }}
        pre {{ background: #161b22; border: 1px solid #30363d; border-radius: 8px;
               padding: 1rem; overflow-x: auto; font-size: 0.8rem; }}
        code {{ font-family: "SF Mono", "Fira Code", monospace; }}
        .actions {{ display: flex; gap: 1rem; margin-bottom: 2rem; flex-wrap: wrap; }}
        .btn {{ display: inline-block; padding: 0.5rem 1rem; border-radius: 6px; border: 1px solid #30363d;
                background: #21262d; color: #c9d1d9; cursor: pointer; font-size: 0.875rem;
                font-family: inherit; }}
        .btn:hover {{ background: #30363d; }}
        .btn.primary {{ background: #238636; border-color: #2ea043; color: #fff; }}
        .btn.primary:hover {{ background: #2ea043; }}
        .btn.warning {{ background: #9e6a03; border-color: #d29922; color: #fff; }}
        .btn.warning:hover {{ background: #bb8009; }}
        textarea {{ width: 100%; min-height: 120px; background: #0d1117; border: 1px solid #30363d;
                    border-radius: 6px; color: #c9d1d9; padding: 0.75rem; font-family: "SF Mono", "Fira Code", monospace;
                    font-size: 0.8rem; resize: vertical; }}
        .feedback {{ margin-top: 0.5rem; padding: 0.5rem 0.75rem; border-radius: 6px; font-size: 0.875rem;
                     display: none; }}
        .feedback.success {{ display: block; background: #0d3117; color: #3fb950; border: 1px solid #238636; }}
        .feedback.error {{ display: block; background: #3d1418; color: #f85149; border: 1px solid #da3633; }}
        .filters {{ display: flex; gap: 1rem; align-items: center; flex-wrap: wrap; margin-bottom: 1rem;
                     padding: 0.75rem 1rem; background: #161b22; border: 1px solid #30363d; border-radius: 8px; }}
        .filters label {{ color: #8b949e; font-size: 0.8rem; display: flex; align-items: center; gap: 0.4rem; }}
        .filters select, .filters input[type="date"] {{
            background: #0d1117; border: 1px solid #30363d; border-radius: 4px; color: #c9d1d9;
            padding: 0.3rem 0.5rem; font-size: 0.8rem; font-family: inherit; }}
    </style>
</head>
<body>
    <div class="container">
        <div class="breadcrumb"><a href="/">Fleet Dashboard</a> / {hostname}</div>
        <h1>{hostname}</h1>
        <p class="subtitle">{device_id}</p>

        <div class="meta">
            <div class="meta-item">
                <div class="label">Status</div>
                <div class="value"><span class="status {status_class}">{status}</span></div>
            </div>
            <div class="meta-item">
                <div class="label">OS / Architecture</div>
                <div class="value">{os} / {arch}</div>
            </div>
            <div class="meta-item">
                <div class="label">Last Check-in</div>
                <div class="value">{last_checkin}</div>
            </div>
            <div class="meta-item">
                <div class="label">Config Hash</div>
                <div class="value"><code>{config_hash}</code></div>
            </div>
        </div>

        <div class="section">
            <h2>Actions</h2>
            <div class="actions">
                <button class="btn warning" onclick="forceReconcile()">Force Reconcile</button>
            </div>
            <div id="action-feedback" class="feedback"></div>
        </div>

        {desired_config_html}

        <div class="section">
            <h2>Push Configuration</h2>
            <p class="muted" style="margin-bottom: 0.75rem;">Enter JSON configuration to push to this device.</p>
            <textarea id="config-input" placeholder='{{"packages": ["vim", "git"], "files": []}}'></textarea>
            <div style="margin-top: 0.75rem;">
                <button class="btn primary" onclick="pushConfig()">Push Config</button>
            </div>
            <div id="config-feedback" class="feedback"></div>
        </div>

        <div class="section">
            <h2>Event History</h2>
            <div class="filters">
                <label>Type:
                    <select id="filter-type" onchange="applyFilters()">
                        <option value="all">All</option>
                        <option value="drift">Drift</option>
                        <option value="checkin">Check-in</option>
                    </select>
                </label>
                <label>From:
                    <input type="date" id="filter-from" onchange="applyFilters()">
                </label>
                <label>To:
                    <input type="date" id="filter-to" onchange="applyFilters()">
                </label>
                <button class="btn" onclick="clearFilters()" style="margin-left:0.5rem;">Clear</button>
            </div>

            <h3 style="color:#c9d1d9;font-size:1rem;margin:1rem 0 0.5rem;">Drift Events ({drift_count})</h3>
            <div id="drift-section">
            {drift_html}
            </div>

            <h3 style="color:#c9d1d9;font-size:1rem;margin:1.5rem 0 0.5rem;">Check-in History ({checkin_count})</h3>
            <div id="checkin-section">
            {checkin_html}
            </div>
        </div>
    </div>
    <script>
        var deviceId = "{device_id_js}";
        function getAuthHeader() {{
            // If CFGD_API_KEY is set on the server, users must provide a token.
            // For the web UI, we read it from localStorage if available.
            var token = localStorage.getItem("cfgd_api_token");
            if (token) return {{"Authorization": "Bearer " + token}};
            return {{}};
        }}
        function showFeedback(elId, msg, isError) {{
            var el = document.getElementById(elId);
            el.textContent = msg;
            el.className = "feedback " + (isError ? "error" : "success");
            setTimeout(function() {{ el.className = "feedback"; }}, 5000);
        }}
        function forceReconcile() {{
            fetch("/api/v1/devices/" + encodeURIComponent(deviceId) + "/reconcile", {{
                method: "POST",
                headers: getAuthHeader()
            }}).then(function(r) {{
                if (r.ok) {{
                    showFeedback("action-feedback", "Force reconcile requested. Device will reconcile on next check-in.", false);
                    setTimeout(function() {{ location.reload(); }}, 1500);
                }} else {{
                    return r.text().then(function(t) {{ throw new Error(t); }});
                }}
            }}).catch(function(e) {{
                showFeedback("action-feedback", "Error: " + e.message, true);
            }});
        }}
        function pushConfig() {{
            var input = document.getElementById("config-input").value.trim();
            if (!input) {{
                showFeedback("config-feedback", "Please enter a JSON configuration.", true);
                return;
            }}
            try {{ JSON.parse(input); }} catch(e) {{
                showFeedback("config-feedback", "Invalid JSON: " + e.message, true);
                return;
            }}
            var headers = getAuthHeader();
            headers["Content-Type"] = "application/json";
            fetch("/api/v1/devices/" + encodeURIComponent(deviceId) + "/config", {{
                method: "PUT",
                headers: headers,
                body: JSON.stringify({{ config: JSON.parse(input) }})
            }}).then(function(r) {{
                if (r.ok) {{
                    showFeedback("config-feedback", "Configuration pushed successfully.", false);
                    setTimeout(function() {{ location.reload(); }}, 1500);
                }} else {{
                    return r.text().then(function(t) {{ throw new Error(t); }});
                }}
            }}).catch(function(e) {{
                showFeedback("config-feedback", "Error: " + e.message, true);
            }});
        }}
        function applyFilters() {{
            var typeFilter = document.getElementById("filter-type").value;
            var fromDate = document.getElementById("filter-from").value;
            var toDate = document.getElementById("filter-to").value;
            var driftSection = document.getElementById("drift-section");
            var checkinSection = document.getElementById("checkin-section");
            // Show/hide sections based on type filter
            driftSection.style.display = (typeFilter === "all" || typeFilter === "drift") ? "" : "none";
            checkinSection.style.display = (typeFilter === "all" || typeFilter === "checkin") ? "" : "none";
            // Date filter: hide rows outside range
            filterTableRows(driftSection, fromDate, toDate);
            filterTableRows(checkinSection, fromDate, toDate);
        }}
        function filterTableRows(section, fromDate, toDate) {{
            var rows = section.querySelectorAll("tbody tr");
            for (var i = 0; i < rows.length; i++) {{
                var ts = rows[i].querySelector("td").textContent.trim();
                var date = ts.substring(0, 10);
                var show = true;
                if (fromDate && date < fromDate) show = false;
                if (toDate && date > toDate) show = false;
                rows[i].style.display = show ? "" : "none";
            }}
        }}
        function clearFilters() {{
            document.getElementById("filter-type").value = "all";
            document.getElementById("filter-from").value = "";
            document.getElementById("filter-to").value = "";
            applyFilters();
        }}
    </script>
</body>
</html>"#,
        styles = COMMON_STYLES,
        hostname = xml_escape(&device.hostname),
        device_id = xml_escape(&device.id),
        device_id_js = device_id_js,
        status_class = status_class,
        status = xml_escape(device.status.as_str()),
        os = xml_escape(&device.os),
        arch = xml_escape(&device.arch),
        last_checkin = xml_escape(&device.last_checkin),
        config_hash = xml_escape(&device.config_hash),
        desired_config_html = desired_config_html,
        drift_count = drift_events.len(),
        drift_html = drift_html,
        checkin_count = checkin_events.len(),
        checkin_html = checkin_html,
    );

    Ok(Html(html))
}

async fn fleet_events(State(state): State<SharedState>) -> Result<Html<String>, GatewayError> {
    let db = state.db.lock().await;
    let events = db.list_fleet_events(200)?;

    let mut event_rows = String::new();
    for e in &events {
        let type_class = match e.event_type.as_str() {
            "drift" => "drifted",
            "config-changed" => "drifted",
            _ => "healthy",
        };
        let summary_display = if e.event_type == "drift" {
            let parsed: Vec<serde_json::Value> =
                serde_json::from_str(&e.summary).unwrap_or_default();
            if parsed.is_empty() {
                xml_escape(&e.summary)
            } else {
                parsed
                    .iter()
                    .map(|d| {
                        let field = d.get("field").and_then(|v| v.as_str()).unwrap_or("?");
                        let expected = d.get("expected").and_then(|v| v.as_str()).unwrap_or("?");
                        let actual = d.get("actual").and_then(|v| v.as_str()).unwrap_or("?");
                        format!(
                            "{}: {} &rarr; {}",
                            xml_escape(field),
                            xml_escape(expected),
                            xml_escape(actual)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        } else {
            format!("<code>{}</code>", xml_escape(&e.summary))
        };
        event_rows.push_str(&format!(
            r#"<tr>
                <td>{timestamp}</td>
                <td><a href="/devices/{device_id_raw}">{device_id}</a></td>
                <td><span class="status {type_class}">{event_type}</span></td>
                <td>{summary}</td>
            </tr>"#,
            timestamp = xml_escape(&e.timestamp),
            device_id_raw = xml_escape(&e.device_id),
            device_id = xml_escape(&e.device_id),
            type_class = type_class,
            event_type = xml_escape(&e.event_type),
            summary = summary_display,
        ));
    }

    let events_table = if events.is_empty() {
        r#"<div class="empty">No events recorded yet. Events will appear here as devices check in and report drift.</div>"#.to_string()
    } else {
        format!(
            r#"<table>
            <thead>
                <tr>
                    <th>Timestamp</th>
                    <th>Device</th>
                    <th>Type</th>
                    <th>Summary</th>
                </tr>
            </thead>
            <tbody>
                {event_rows}
            </tbody>
        </table>"#
        )
    };

    // Collect unique device IDs for the filter dropdown
    let mut device_ids: Vec<String> = events.iter().map(|e| e.device_id.clone()).collect();
    device_ids.sort();
    device_ids.dedup();
    let device_options: String = device_ids
        .iter()
        .map(|id| format!(r#"<option value="{id}">{id}</option>"#, id = xml_escape(id)))
        .collect::<Vec<_>>()
        .join("\n");

    let html = format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>cfgd - Fleet Events</title>
    <style>
        {styles}
        .filters {{ display: flex; gap: 1rem; align-items: center; flex-wrap: wrap; margin-bottom: 1rem;
                     padding: 0.75rem 1rem; background: #161b22; border: 1px solid #30363d; border-radius: 8px; }}
        .filters label {{ color: #8b949e; font-size: 0.8rem; display: flex; align-items: center; gap: 0.4rem; }}
        .filters select {{ background: #0d1117; border: 1px solid #30363d; border-radius: 4px; color: #c9d1d9;
                           padding: 0.3rem 0.5rem; font-size: 0.8rem; font-family: inherit; }}
        .btn {{ display: inline-block; padding: 0.3rem 0.75rem; border-radius: 6px; border: 1px solid #30363d;
                background: #21262d; color: #c9d1d9; cursor: pointer; font-size: 0.8rem; font-family: inherit; }}
        .btn:hover {{ background: #30363d; }}
        .live-badge {{ display: inline-block; padding: 0.1rem 0.4rem; border-radius: 4px; font-size: 0.7rem;
                       font-weight: 600; text-transform: uppercase; margin-left: 0.5rem; }}
        .live-badge.connected {{ background: #0d3117; color: #3fb950; }}
        .live-badge.disconnected {{ background: #3d1418; color: #f85149; }}
    </style>
</head>
<body>
    <div class="container">
        <h1>cfgd Fleet Events <span id="sse-badge" class="live-badge disconnected">connecting</span></h1>
        <p class="subtitle">Unified timeline of check-ins, config changes, and drift events</p>
        <div class="nav">
            <a href="/">Devices</a>
            <a href="/events" class="active">Events</a>
        </div>
        <div class="filters">
            <label>Device:
                <select id="filter-device" onchange="applyFleetFilters()">
                    <option value="all">All</option>
                    {device_options}
                </select>
            </label>
            <label>Type:
                <select id="filter-type" onchange="applyFleetFilters()">
                    <option value="all">All</option>
                    <option value="checkin">Check-in</option>
                    <option value="config-changed">Config Changed</option>
                    <option value="drift">Drift</option>
                </select>
            </label>
            <button class="btn" onclick="clearFleetFilters()">Clear</button>
        </div>
        <div id="events-container">
        {events_table}
        </div>
    </div>
    <script>
        function applyFleetFilters() {{
            var deviceFilter = document.getElementById("filter-device").value;
            var typeFilter = document.getElementById("filter-type").value;
            var rows = document.querySelectorAll("#events-container tbody tr");
            for (var i = 0; i < rows.length; i++) {{
                var cells = rows[i].querySelectorAll("td");
                if (cells.length < 3) continue;
                var device = cells[1].textContent.trim();
                var evType = cells[2].textContent.trim().toLowerCase();
                var show = true;
                if (deviceFilter !== "all" && device !== deviceFilter) show = false;
                if (typeFilter !== "all" && evType !== typeFilter) show = false;
                rows[i].style.display = show ? "" : "none";
            }}
        }}
        function clearFleetFilters() {{
            document.getElementById("filter-device").value = "all";
            document.getElementById("filter-type").value = "all";
            applyFleetFilters();
        }}
        // SSE live updates
        function typeClass(t) {{
            if (t === "drift" || t === "config-changed") return "drifted";
            return "healthy";
        }}
        function escapeHtml(s) {{
            var d = document.createElement("div");
            d.textContent = s;
            return d.innerHTML;
        }}
        function connectSSE() {{
            var badge = document.getElementById("sse-badge");
            var source = new EventSource("/api/v1/events/stream");
            source.onopen = function() {{
                badge.textContent = "live";
                badge.className = "live-badge connected";
            }};
            source.onerror = function() {{
                badge.textContent = "disconnected";
                badge.className = "live-badge disconnected";
            }};
            function handleEvent(e) {{
                var data = JSON.parse(e.data);
                var tbody = document.querySelector("#events-container tbody");
                if (!tbody) {{
                    var container = document.getElementById("events-container");
                    container.innerHTML = '<table><thead><tr><th>Timestamp</th><th>Device</th><th>Type</th><th>Summary</th></tr></thead><tbody></tbody></table>';
                    tbody = container.querySelector("tbody");
                }}
                var cls = typeClass(data["event-type"] || data.event_type);
                var evType = data["event-type"] || data.event_type;
                var summary = data.summary || "";
                var deviceId = data["device-id"] || data.device_id;
                if (evType !== "drift") {{
                    summary = "<code>" + escapeHtml(summary) + "</code>";
                }} else {{
                    summary = escapeHtml(summary);
                }}
                var row = document.createElement("tr");
                row.innerHTML = '<td>' + escapeHtml(data.timestamp) + '</td>'
                    + '<td><a href="/devices/' + encodeURIComponent(deviceId) + '">' + escapeHtml(deviceId) + '</a></td>'
                    + '<td><span class="status ' + cls + '">' + escapeHtml(evType) + '</span></td>'
                    + '<td>' + summary + '</td>';
                tbody.insertBefore(row, tbody.firstChild);
                var deviceSelect = document.getElementById("filter-device");
                var exists = false;
                for (var i = 0; i < deviceSelect.options.length; i++) {{
                    if (deviceSelect.options[i].value === deviceId) {{ exists = true; break; }}
                }}
                if (!exists) {{
                    var opt = document.createElement("option");
                    opt.value = deviceId;
                    opt.textContent = deviceId;
                    deviceSelect.appendChild(opt);
                }}
                applyFleetFilters();
            }}
            source.addEventListener("checkin", handleEvent);
            source.addEventListener("config-changed", handleEvent);
            source.addEventListener("drift", handleEvent);
        }}
        connectSSE();
    </script>
</body>
</html>"##,
        styles = COMMON_STYLES,
        device_options = device_options,
        events_table = events_table,
    );

    Ok(Html(html))
}

fn status_to_class(status: &str) -> &'static str {
    match status {
        "healthy" => "healthy",
        "drifted" => "drifted",
        "pending-reconcile" => "pending-reconcile",
        _ => "offline",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use axum::middleware;
    use axum::routing::get;
    use tower::ServiceExt;

    use super::super::api::{AppState, EnrollmentMethod};
    use super::super::db::ServerDb;

    fn test_state() -> SharedState {
        let db = ServerDb::open(":memory:").expect("open in-memory db");
        let (event_tx, _) = tokio::sync::broadcast::channel(16);
        AppState {
            db: Arc::new(tokio::sync::Mutex::new(db)),
            kube_client: None,
            event_tx,
            enrollment_method: EnrollmentMethod::Token,
            metrics: None,
        }
    }

    // --- status_to_class ---

    #[test]
    fn status_to_class_mapping() {
        let cases: &[(&str, &str)] = &[
            ("healthy", "healthy"),
            ("drifted", "drifted"),
            ("pending-reconcile", "pending-reconcile"),
            ("offline", "offline"),
            ("something-else", "offline"),
        ];
        for (input, expected) in cases {
            assert_eq!(status_to_class(input), *expected, "failed for {input:?}");
        }
    }

    // --- dashboard ---

    #[tokio::test]
    async fn dashboard_empty_device_list() {
        let state = test_state();
        let result = dashboard(State(state)).await;
        assert!(result.is_ok());
        let html = result.unwrap().0;
        assert!(html.contains("cfgd Fleet Dashboard"));
        assert!(html.contains("Configuration management control plane"));
        assert!(html.contains("No devices registered yet"));
        assert!(html.contains("Total Devices"));
        // All stats should be 0
        assert!(html.contains(r#"<div class="value">0</div>"#));
        // Nav links present
        assert!(
            html.contains(r#"<a href="/">Devices</a>"#)
                || html.contains(r#"<a href="/" class="active">Devices</a>"#)
        );
        assert!(html.contains(r#"<a href="/events">Events</a>"#));
    }

    #[tokio::test]
    async fn dashboard_with_devices_shows_device_rows() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "workstation-1", "linux", "x86_64", "abc123", None)
                .expect("register device");
            db.register_device("dev-2", "laptop-2", "darwin", "aarch64", "def456", None)
                .expect("register device");
        }
        let result = dashboard(State(state)).await;
        assert!(result.is_ok());
        let html = result.unwrap().0;
        // Device names appear in table
        assert!(html.contains("workstation-1"));
        assert!(html.contains("laptop-2"));
        // Device IDs appear
        assert!(html.contains("dev-1"));
        assert!(html.contains("dev-2"));
        // OS/arch values appear
        assert!(html.contains("linux"));
        assert!(html.contains("darwin"));
        assert!(html.contains("x86_64"));
        assert!(html.contains("aarch64"));
        // Config hashes appear
        assert!(html.contains("abc123"));
        assert!(html.contains("def456"));
        // Should have a table, not the "no devices" message
        assert!(html.contains("<table"));
        assert!(!html.contains("No devices registered yet"));
        // Stats: 2 total, 2 healthy (register_device sets Healthy)
        assert!(html.contains(r#"<div class="value">2</div>"#));
    }

    #[tokio::test]
    async fn dashboard_stat_cards_reflect_device_statuses() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("d1", "host1", "linux", "x86_64", "h1", None)
                .expect("register");
            db.register_device("d2", "host2", "linux", "x86_64", "h2", None)
                .expect("register");
            // Cause drift on d2
            db.record_drift_event("d2", "field changed").expect("drift");
        }
        let result = dashboard(State(state)).await;
        let html = result.unwrap().0;
        // Total = 2, Healthy = 1, Drifted = 1, Offline = 0
        // The stat cards are: total, healthy, drifted, offline in that order
        // We check the stat card structure
        assert!(html.contains(r#"<div class="stat-card total">"#));
        assert!(html.contains(r#"<div class="stat-card healthy">"#));
        assert!(html.contains(r#"<div class="stat-card drifted">"#));
        assert!(html.contains(r#"<div class="stat-card offline">"#));
        // Check the status badge on the drifted device
        assert!(html.contains(r#"<span class="status drifted">drifted</span>"#));
        // And the healthy device
        assert!(html.contains(r#"<span class="status healthy">healthy</span>"#));
    }

    #[tokio::test]
    async fn dashboard_escapes_html_in_device_fields() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device(
                "dev-<xss>",
                "host<script>",
                "linux&os",
                "x86\"64",
                "hash'val",
                None,
            )
            .expect("register");
        }
        let result = dashboard(State(state)).await;
        let html = result.unwrap().0;
        // XSS characters should be escaped
        assert!(html.contains("dev-&lt;xss&gt;"));
        assert!(html.contains("host&lt;script&gt;"));
        assert!(html.contains("linux&amp;os"));
        assert!(html.contains("x86&quot;64"));
        assert!(html.contains("hash&apos;val"));
        // Raw dangerous characters should NOT appear
        assert!(!html.contains("<xss>"));
        assert!(!html.contains("<script>"));
    }

    // --- device_detail ---

    #[tokio::test]
    async fn device_detail_existing_device() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device(
                "dev-42",
                "my-workstation",
                "linux",
                "x86_64",
                "abc123",
                None,
            )
            .expect("register");
        }
        let result = device_detail(State(state), Path("dev-42".to_string())).await;
        assert!(result.is_ok());
        let html = result.unwrap().0;
        // Device details in the page
        assert!(html.contains("my-workstation"));
        assert!(html.contains("dev-42"));
        assert!(html.contains("linux"));
        assert!(html.contains("x86_64"));
        assert!(html.contains("abc123"));
        // Breadcrumb navigation
        assert!(html.contains("Fleet Dashboard"));
        // No drift/checkin events by default
        assert!(html.contains("No drift events recorded"));
        assert!(html.contains("No check-in events recorded"));
        // Push config section
        assert!(html.contains("Push Configuration"));
        // Actions section
        assert!(html.contains("Force Reconcile"));
    }

    #[tokio::test]
    async fn device_detail_not_found_returns_error() {
        let state = test_state();
        let result = device_detail(State(state), Path("nonexistent".to_string())).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, GatewayError::NotFound(_)));
    }

    #[tokio::test]
    async fn device_detail_with_drift_events() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
                .expect("register");
            db.record_drift_event(
                "dev-1",
                r#"[{"field":"packages","expected":"vim","actual":"missing"}]"#,
            )
            .expect("drift");
        }
        let result = device_detail(State(state), Path("dev-1".to_string())).await;
        let html = result.unwrap().0;
        // Should not say "no drift events"
        assert!(!html.contains("No drift events recorded"));
        // Should contain parsed drift detail fields
        assert!(html.contains("packages"));
        assert!(html.contains("vim"));
        assert!(html.contains("missing"));
        // Drift events count
        assert!(html.contains("Drift Events (1)"));
    }

    #[tokio::test]
    async fn device_detail_with_checkin_events() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
                .expect("register");
            db.record_checkin("dev-1", "hash-abc", false)
                .expect("checkin");
            db.record_checkin("dev-1", "hash-def", true)
                .expect("checkin changed");
        }
        let result = device_detail(State(state), Path("dev-1".to_string())).await;
        let html = result.unwrap().0;
        // Should not say "no check-in events"
        assert!(!html.contains("No check-in events recorded"));
        // Config hashes appear
        assert!(html.contains("hash-abc"));
        assert!(html.contains("hash-def"));
        // Changed badge for the config-changed checkin
        assert!(html.contains(r#"<span class="status drifted">changed</span>"#));
        // OK badge for the unchanged checkin
        assert!(html.contains(r#"<span class="status healthy">ok</span>"#));
        // Checkin count
        assert!(html.contains("Check-in History (2)"));
    }

    #[tokio::test]
    async fn device_detail_with_desired_config() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
                .expect("register");
            let config = serde_json::json!({"packages": ["vim", "git"]});
            db.set_device_config("dev-1", &config).expect("set config");
        }
        let result = device_detail(State(state), Path("dev-1".to_string())).await;
        let html = result.unwrap().0;
        // Desired config section should show the formatted JSON
        assert!(html.contains("Desired Configuration"));
        assert!(html.contains("vim"));
        assert!(html.contains("git"));
        // Should NOT contain the "no desired config" message
        assert!(!html.contains("No desired config set"));
    }

    #[tokio::test]
    async fn device_detail_without_desired_config() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
                .expect("register");
        }
        let result = device_detail(State(state), Path("dev-1".to_string())).await;
        let html = result.unwrap().0;
        assert!(html.contains("No desired config set"));
    }

    #[tokio::test]
    async fn device_detail_escapes_html_in_fields() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device(
                "dev-<id>",
                "host<name>",
                "os&type",
                "arch\"val",
                "hash'v",
                None,
            )
            .expect("register");
        }
        let result = device_detail(State(state), Path("dev-<id>".to_string())).await;
        let html = result.unwrap().0;
        assert!(html.contains("dev-&lt;id&gt;"));
        assert!(html.contains("host&lt;name&gt;"));
        assert!(html.contains("os&amp;type"));
        assert!(!html.contains("host<name>"));
    }

    // --- fleet_events ---

    #[tokio::test]
    async fn fleet_events_empty() {
        let state = test_state();
        let result = fleet_events(State(state)).await;
        assert!(result.is_ok());
        let html = result.unwrap().0;
        assert!(html.contains("cfgd Fleet Events"));
        assert!(html.contains("No events recorded yet"));
        assert!(html.contains("Events</a>"));
        assert!(html.contains("Devices</a>"));
    }

    #[tokio::test]
    async fn fleet_events_with_checkin_events() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
                .expect("register");
            db.record_checkin("dev-1", "hash-abc", false)
                .expect("checkin");
        }
        let result = fleet_events(State(state)).await;
        let html = result.unwrap().0;
        // Should have a table, not the empty message
        assert!(!html.contains("No events recorded yet"));
        assert!(html.contains("<table"));
        // Device link
        assert!(html.contains(r#"<a href="/devices/dev-1">dev-1</a>"#));
        // Checkin event type
        assert!(html.contains("checkin"));
        // Config hash in summary
        assert!(html.contains("hash-abc"));
    }

    #[tokio::test]
    async fn fleet_events_with_drift_events() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
                .expect("register");
            db.record_drift_event(
                "dev-1",
                r#"[{"field":"sysctl","expected":"1","actual":"0"}]"#,
            )
            .expect("drift");
        }
        let result = fleet_events(State(state)).await;
        let html = result.unwrap().0;
        assert!(html.contains("drift"));
        assert!(html.contains("sysctl"));
        // Drift events get the "drifted" status class
        assert!(html.contains(r#"class="status drifted"#));
        // Device ID link
        assert!(html.contains("dev-1"));
    }

    #[tokio::test]
    async fn fleet_events_with_config_changed_events() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-1", "host-1", "linux", "x86_64", "h1", None)
                .expect("register");
            db.record_checkin("dev-1", "new-hash", true)
                .expect("changed checkin");
        }
        let result = fleet_events(State(state)).await;
        let html = result.unwrap().0;
        assert!(html.contains("config-changed"));
        // Config-changed events also get the "drifted" type class
        assert!(html.contains(r#"class="status drifted"#));
    }

    #[tokio::test]
    async fn fleet_events_device_filter_dropdown_populated() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-a", "host-a", "linux", "x86_64", "h1", None)
                .expect("register");
            db.register_device("dev-b", "host-b", "linux", "x86_64", "h2", None)
                .expect("register");
            db.record_checkin("dev-a", "h1", false).expect("checkin");
            db.record_checkin("dev-b", "h2", false).expect("checkin");
        }
        let result = fleet_events(State(state)).await;
        let html = result.unwrap().0;
        // Device filter dropdown should contain both device IDs as options
        assert!(html.contains(r#"<option value="dev-a">dev-a</option>"#));
        assert!(html.contains(r#"<option value="dev-b">dev-b</option>"#));
    }

    #[tokio::test]
    async fn fleet_events_escapes_html_in_device_ids() {
        let state = test_state();
        {
            let db = state.db.lock().await;
            db.register_device("dev-<x>", "host", "linux", "x86_64", "h", None)
                .expect("register");
            db.record_checkin("dev-<x>", "h", false).expect("checkin");
        }
        let result = fleet_events(State(state)).await;
        let html = result.unwrap().0;
        assert!(html.contains("dev-&lt;x&gt;"));
        assert!(!html.contains("dev-<x>"));
    }

    // --- fleet_events SSE badge ---

    #[tokio::test]
    async fn fleet_events_contains_sse_badge() {
        let state = test_state();
        let result = fleet_events(State(state)).await;
        let html = result.unwrap().0;
        assert!(html.contains("sse-badge"));
        assert!(html.contains("EventSource"));
        assert!(html.contains("/api/v1/events/stream"));
    }

    // --- web_auth_middleware ---

    /// Build a minimal router with the auth middleware for testing.
    fn auth_test_app() -> axum::Router {
        axum::Router::new()
            .route("/test", get(|| async { "ok" }))
            .route_layer(middleware::from_fn(web_auth_middleware))
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn auth_middleware_allows_when_no_api_key_set() {
        // Ensure CFGD_API_KEY is not set
        unsafe { std::env::remove_var("CFGD_API_KEY") };

        let app = auth_test_app();
        let resp = app
            .oneshot(Request::builder().uri("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn auth_middleware_rejects_without_credentials_when_key_set() {
        unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

        let app = auth_test_app();
        let resp = app
            .oneshot(Request::builder().uri("/test").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        unsafe { std::env::remove_var("CFGD_API_KEY") };
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn auth_middleware_accepts_valid_bearer_token() {
        unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

        let app = auth_test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("Authorization", "Bearer test-secret-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        unsafe { std::env::remove_var("CFGD_API_KEY") };
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn auth_middleware_rejects_wrong_bearer_token() {
        unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

        let app = auth_test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header("Authorization", "Bearer wrong-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        unsafe { std::env::remove_var("CFGD_API_KEY") };
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn auth_middleware_accepts_valid_session_cookie() {
        unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

        let app = auth_test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header(header::COOKIE, "cfgd_session=test-secret-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        unsafe { std::env::remove_var("CFGD_API_KEY") };
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn auth_middleware_rejects_wrong_session_cookie() {
        unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

        let app = auth_test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header(header::COOKIE, "cfgd_session=wrong-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        unsafe { std::env::remove_var("CFGD_API_KEY") };
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn auth_middleware_accepts_cookie_among_multiple() {
        unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

        let app = auth_test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/test")
                    .header(
                        header::COOKIE,
                        "other=value; cfgd_session=test-secret-key; another=thing",
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        unsafe { std::env::remove_var("CFGD_API_KEY") };
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn auth_middleware_token_query_param_redirects_and_sets_cookie() {
        unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

        let app = auth_test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/test?token=test-secret-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // Should redirect (303 See Other)
        assert_eq!(resp.status(), StatusCode::SEE_OTHER);
        // Location header should strip the token query param
        let location = resp
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(location, "/test");
        // Set-Cookie header should contain the session cookie
        let set_cookie = resp
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(set_cookie.contains("cfgd_session=test-secret-key"));
        assert!(set_cookie.contains("HttpOnly"));
        assert!(set_cookie.contains("SameSite=Strict"));
        assert!(set_cookie.contains("Max-Age=86400"));

        unsafe { std::env::remove_var("CFGD_API_KEY") };
    }

    #[tokio::test]
    #[serial_test::serial]
    async fn auth_middleware_wrong_token_query_param_rejected() {
        unsafe { std::env::set_var("CFGD_API_KEY", "test-secret-key") };

        let app = auth_test_app();
        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/test?token=wrong-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        unsafe { std::env::remove_var("CFGD_API_KEY") };
    }

    // --- COMMON_STYLES ---

    #[test]
    fn common_styles_contains_expected_classes() {
        assert!(COMMON_STYLES.contains(".status.healthy"));
        assert!(COMMON_STYLES.contains(".status.drifted"));
        assert!(COMMON_STYLES.contains(".status.offline"));
        assert!(COMMON_STYLES.contains(".status.pending-reconcile"));
        assert!(COMMON_STYLES.contains("table"));
        assert!(COMMON_STYLES.contains(".container"));
        assert!(COMMON_STYLES.contains(".nav"));
        assert!(COMMON_STYLES.contains(".empty"));
    }

    // --- router ---

    #[tokio::test]
    async fn router_wires_routes() {
        let state = test_state();
        let app = router().with_state(state);

        // Ensure CFGD_API_KEY is not set so auth middleware lets us through
        unsafe { std::env::remove_var("CFGD_API_KEY") };

        // Dashboard route
        let resp = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        // Events route
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}
