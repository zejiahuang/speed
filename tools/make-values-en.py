#!/usr/bin/env python3
"""Generate `res/values-en/strings.xml` from the Chinese source.

Why a script rather than a hand-written file: the two documents have to stay in
lockstep. This one copies every comment and every `<string>` element from the
source and replaces only the text, then *proves* the two files agree — same set
of names, same format specifiers in each string. A hand-written translation
drifts the first time someone adds a string and forgets the other file; this
fails loudly instead.

Escaping: Android wants `'` written as `\\'` and `"` as `\\"` inside a string
resource (an unescaped apostrophe is a build error, not a warning), and `&` as
`&amp;`. The English text below is written naturally and escaped here, so the
dictionary stays readable.
"""

from __future__ import annotations

import io
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SRC = ROOT / "android/app/src/main/res/values/strings.xml"
DST = ROOT / "android/app/src/main/res/values-en/strings.xml"

# name -> English text, unescaped. Order here does not matter; the file order
# comes from the source.
EN: dict[str, str] = {
    # Bottom navigation
    "nav_home": "Connect",
    "nav_rules": "Rules",
    "nav_logs": "Log",
    "nav_settings": "Settings",

    # Home
    "home_state_off": "Not connected",
    "home_state_connecting": "Connecting…",
    "home_state_on": "Connected",
    "home_state_error": "Connection failed",
    "home_action_connect": "Connect",
    "home_action_disconnect": "Disconnect",
    "home_mode_proxy": "Proxy mode",
    "home_mode_vpn": "VPN mode",
    "home_uptime": "Up for %1$s",
    "home_speed_down": "Down",
    "home_speed_up": "Up",
    "home_total": "This session",
    "home_domains_saved": "Rules hit %1$d times this session",
    "home_no_permission": "VPN mode needs system authorisation",
    "home_grant": "Grant",
    "home_switch_mode": "Switch mode",
    "home_proxy_address": "Proxy address",
    "home_copy": "Copy",
    "home_copied": "Copied",
    "home_proxy_how": "Put this address into the app that should use the proxy",
    "home_proxy_limits": "Timeouts, upstream egress and the upstream resolver apply to VPN mode only",
    "home_mode_pick": "Choose a mode",
    "home_mode_root": "Root mode",
    "home_mode_proxy_desc": "Root-free, no system authorisation. Affects only apps that can be pointed at an HTTP proxy.",
    "home_mode_vpn_desc": "Takes over public traffic and needs system VPN authorisation. The full feature set.",
    "home_mode_root_desc": "No VPN authorisation: rewrites the system hosts file and redirects traffic to a local proxy with iptables. Needs root, and no longer validates upstream certificates.",
    "home_error_hint": "Connection failed. Check whether the rules have been downloaded, or try another mode.",
    "home_disconnect_confirm_title": "Disconnect?",
    "home_disconnect_confirm_text": "Data in flight is cut off immediately.",
    "home_disconnect_confirm_ok": "Disconnect",

    # Root mode
    "root_consent_title": "Enter root mode?",
    "root_consent_text": "Root mode terminates TLS on this device and connects to the real server as a man in the middle: the certificate the client sees is issued by this app, and upstream certificates are no longer validated. Enabling it needs root, rewrites the system hosts file and installs a self-signed CA into the system trust store; disconnecting reverts all of it.",
    "root_consent_ok": "I understand, continue",
    "root_unavailable": "No usable root access",
    "root_ca_failed": "Cannot read the device CA certificate",

    # Rules
    "rules_title": "Rules",
    "rules_search_hint": "Search domains, addresses or groups",
    "rules_enabled_summary": "%1$d of %2$d groups enabled",
    "rules_refresh": "Refresh rules",
    "rules_source": "Source",
    "rules_source_github_hosts": "github-hosts",
    "rules_source_hellogithub": "HelloGitHub (GitHub520)",
    "rules_source_s302_upstream": "s302 substitute upstreams",
    "rules_source_root_pinned": "Root mode is pinned to this source; switch to VPN or proxy mode to choose another",
    "rules_add_source": "Add a custom source",
    "rules_source_name": "Name (optional)",
    "rules_source_url": "Address",
    "rules_source_add_ok": "Add",
    "rules_source_delete": "Delete",
    "rules_source_unavailable": "Unavailable",
    "rules_source_invalid_url": "Enter an address starting with http(s)://",
    "rules_import_local": "Import a local file",
    "rules_source_local": "Local file",
    "rules_import_done": "Imported %1$s",
    "rules_import_already": "The contents of %1$s had already been imported; switched to it",
    "rules_import_failed": "Import failed: %1$s",
    "rules_count_domains": "%1$d domains",
    "rules_count_addresses": "%1$d addresses",
    "rules_expand": "Expand",
    "rules_collapse": "Collapse",
    "rules_empty": "No matching entries",
    "rules_level_group": "Group",
    "rules_level_domain": "Domain",
    "rules_level_address": "Address",
    "rules_disabled_hint": "Rules that are off never reach the kernel",
    "rules_enable_all": "Enable all",
    "rules_disable_all": "Disable all",
    "rules_cache_info": "Cached %1$d KiB · %2$s",
    "rules_switches_applied": "%1$d switches applied",

    # Logs
    "logs_title": "Log",
    "logs_tab_live": "Live",
    "logs_tab_archive": "Archive",
    "logs_pause": "Pause",
    "logs_resume": "Resume",
    "logs_clear": "Clear",
    "logs_export": "Export",
    "logs_empty": "No log entries yet",
    "logs_filter_all": "All",
    "logs_filter_session": "Session",
    "logs_filter_error": "Errors",
    "logs_filter_dns": "DNS",
    "logs_archived_days": "%1$d days archived",
    "logs_no_archive": "Nothing archived yet",

    # Settings
    "settings_title": "Settings",
    "settings_section_connection": "Connection and sources",
    "settings_section_logs": "Log and diagnostics",
    "settings_section_appearance": "Appearance",
    "settings_section_advanced": "Advanced: kernel parameters",
    "settings_section_advanced_shell": "Advanced",
    "settings_page_connection": "Rule source, upstream egress and resolver",
    "settings_page_connection_proxy": "Proxy port, rule source and connection behaviour",
    "settings_page_connection_root": "Rule source (fixed by the mode) and connection behaviour",
    "settings_page_logs": "Stats interval and log archiving",
    "settings_page_appearance": "Theme, glass effect, wallpaper and fonts",
    "settings_page_advanced": "Dialling, timeouts, flow limits and the console",
    "settings_page_advanced_shell": "Developer view, kernel library and console",
    "settings_page_data": "Export, import and version info",
    "settings_back": "Back",
    "settings_proxy_port": "Proxy listening port",
    "settings_proxy_port_hint": "Point the app HTTP proxy at 127.0.0.1:%1$d",
    "settings_rules_url": "Rule source address",
    "settings_refresh_interval": "Refresh interval",
    "settings_offline": "Offline mode",
    "settings_offline_hint": "Use only downloaded rules; no network access",
    "settings_stats_interval": "Stats interval",
    "settings_log_level": "Log level",
    "settings_log_archive": "Archive by day",
    "settings_log_archive_hint": "Keep the last 7 days of logs",
    "settings_clear_logs": "Clear all logs",
    "settings_dynamic_color": "Colours from the wallpaper",
    "settings_dynamic_color_hint": "Available on Android 12 and above",
    "settings_dark_mode": "Dark mode",
    "settings_dark_follow": "Follow system",
    "settings_dark_always": "Always dark",
    "settings_dark_never": "Always light",
    "settings_developer_view": "Developer view",
    "settings_developer_view_hint": "Show kernel details such as candidates, cooldowns and silent counters, and unlock the kernel console",
    "settings_failure_cooldown": "Failure cooldown",
    "settings_max_candidates": "Max candidates per connection",
    "settings_connect_stagger": "Stagger interval",
    "settings_dial_names": "Use dial names",
    "settings_dial_names_hint": "Domains in the rules are re-resolved on refresh",
    "settings_kernel_version": "Kernel version",
    "settings_console": "Kernel console",
    "settings_console_hint": "The same commands as the adb control plane: status, set mtu 1400, dump, help",
    "settings_console_placeholder": "Type a command",
    "settings_console_run": "Run",
    "settings_console_clear": "Clear",
    "settings_console_empty": "No commands run yet",
    "settings_console_running": "Running…",
    "settings_console_prompt": "›",

    # Advanced: kernel config
    "settings_mtu": "Tunnel MTU",
    "settings_mtu_hint": "Lower values mean fewer fragments, at the cost of throughput",
    "settings_connect_timeout": "Per-connection timeout",
    "settings_tcp_idle": "TCP idle reaping",
    "settings_udp_idle": "UDP idle reaping",
    "settings_udp_idle_hint": "Also decides how long a silent UDP flow holds a descriptor",
    "settings_max_tcp_flows": "TCP flow limit",
    "settings_max_udp_flows": "UDP flow limit",
    "settings_answer_dns": "Answer DNS locally",
    "settings_answer_dns_hint": "When a rule matches, the kernel answers directly instead of forwarding",
    "settings_certificate_check": "Validate certificates of rule addresses",
    "settings_certificate_check_hint": "Confirm before dialling that the certificate this address presents covers the target domain. Turning it off is faster, but may connect to an address with the wrong certificate",
    "settings_observe_dns": "Learn addresses from answers",
    "settings_observe_dns_hint": "Record forwarded answers to extend the domain-to-address mapping",
    "settings_race_width": "Race width",
    "settings_race_width_hint": "How many candidates one connection dials at once. 1 disables racing and tries them one at a time",
    "settings_race_launch": "Race launch interval",
    "settings_max_dialing": "Global dialling limit",
    "settings_max_dialing_hint": "Cap on upstream sockets handshaking at once across all connections, so descriptors cannot run out",
    "settings_upstream_proxy": "Upstream egress",
    "settings_upstream_proxy_hint": "Hand public TCP to a proxy to forward. TLS is neither touched nor decrypted; only the egress changes",
    "settings_upstream_proxy_address": "Address",
    "settings_upstream_proxy_address_invalid": "Use IP:port, for example 203.0.113.7:1080. Literal addresses only, no host names",
    "settings_upstream_proxy_kind": "Protocol",
    "settings_upstream_proxy_kind_socks5": "SOCKS5",
    "settings_upstream_proxy_kind_http": "HTTP CONNECT",
    "settings_upstream_proxy_username": "Username",
    "settings_upstream_proxy_password": "Password",
    "settings_upstream_proxy_enable": "Enable upstream egress",
    "settings_upstream_proxy_enable_hint": "Public TCP only; UDP, LAN and loopback addresses still connect directly",
    "settings_dns_upstream": "Upstream resolver",
    "settings_dns_upstream_hint": "Names outside the rule table go to this resolver. Names inside it are unaffected and are still answered locally",
    "settings_dns_upstream_none": "Current resolver",
    "settings_dns_upstream_ali": "AliDNS",
    "settings_dns_upstream_tx": "DNSPod",
    "settings_dns_upstream_cf": "1.1.1.1",
    "settings_dns_upstream_url": "Custom endpoint",
    "settings_dns_upstream_url_hint": "A DoH address starting with https://, for example https://1.1.1.1/dns-query",
    "settings_dns_upstream_url_invalid": "The kernel cannot resolve this endpoint, so it will not be enabled",
    "settings_dns_upstream_address": "Bootstrap address",
    "settings_dns_upstream_address_hint": "Required when the endpoint is a host name: a resolver cannot resolve itself. Use its IP, for example 223.5.5.5",
    "settings_kernel_pending_title": "Kernel parameters changed",
    "settings_kernel_pending_body": "The tunnel is still using the old parameters; the change takes effect only once the kernel is rebuilt.",
    "settings_kernel_pending_action": "Apply and reconnect",

    # Appearance
    "settings_theme_color": "Accent colour",
    "settings_theme_color_dynamic": "From wallpaper",
    "settings_theme_color_brand": "Brand blue",
    "settings_theme_color_green": "Teal",
    "settings_theme_color_orange": "Warm orange",
    "settings_theme_color_violet": "Violet",
    "settings_corner_style": "Corner radius",
    "settings_corner_small": "Compact",
    "settings_corner_medium": "Standard",
    "settings_corner_large": "Rounded",
    "settings_glass": "Glass effect",
    "settings_glass_hint": "Refraction, highlight and rim, with the content behind blurred (needs Android 13; the blur is the expensive part)",
    "settings_glass_blur": "Blur strength",
    "settings_glass_tint": "Tint density",
    "settings_glass_lens": "Refraction strength",
    "settings_glass_highlight": "Highlight",
    "settings_glass_border": "Rim",
    "settings_wallpaper": "Background wallpaper",
    "settings_wallpaper_hint": "A custom app background, which the glass effect refracts",
    "settings_wallpaper_pick": "Choose an image",
    "settings_wallpaper_change": "Replace",
    "settings_wallpaper_clear": "Clear",
    "settings_wallpaper_scrim": "Wallpaper scrim",
    "settings_wallpaper_failed": "The image cannot be read; try another one",
    "settings_font_scale": "Font size",
    "settings_home_show_rate": "Show live rates on the home page",
    "settings_home_show_rate_hint": "Turn it off to save power; rates refresh every 0.5 s",
    "settings_confirm_disconnect": "Confirm before disconnecting",
    "settings_confirm_disconnect_hint": "Avoids cutting a running download with a stray tap",

    # Control
    "control_unavailable": "adb control is not exposed in this build",

    # Relative ages
    "age_just_now": "just now",
    "age_seconds": "%1$d s ago",
    "age_minutes": "%1$d min ago",
    "age_hours": "%1$d h ago",
    "age_days": "%1$d d ago",

    # Home, second batch
    "home_rule_card_title": "Rule status",
    "home_rule_source_label": "Rule source",
    "home_rule_cache_label": "Cache",
    "home_rule_cache_none": "Not cached yet",
    "home_rule_cache_fresh": "Fresh",
    "home_rule_cache_stale": "Stale",
    "home_rule_cache_detail": "%1$s · %2$s",
    "home_hits_title": "Rule hit statistics",
    "home_hits_matched": "Rule hits",
    "home_hits_direct": "Direct",
    "home_hits_live": "Live flows",
    "home_hits_unnamed": "Unnamed traffic",
    "home_hits_named_by_sni": "of which named at handshake",
    "home_hits_named_under_ech": "of which rerouted under ECH",
    "home_hits_hellos_unnamed": "Handshakes left unnamed",
    "home_hits_ratio": "Hit rate %1$s",
    "home_hits_none": "No traffic through the tunnel yet",

    # Developer rows
    "dev_tcp_open_close": "TCP open/close",
    "dev_tcp_failures": "TCP failures",
    "dev_tcp_rejected": "TCP rejected",
    "dev_udp_open_evict": "UDP open/evicted",
    "dev_dns_local": "DNS answered locally",
    "dev_dns_trimmed": "DNS trimmed",
    "dev_dns_upstream": "DNS resolved upstream",
    "dev_live_flows": "Live flows",
    "dev_packets": "Packets in/out",

    # Logs, second batch
    "logs_copy": "Copy",
    "logs_copied": "Copied",
    "logs_export_all": "Export all logs",
    "logs_export_share_title": "Export logs",
    "logs_export_empty": "No logs to export yet",
    "logs_export_done": "Logs exported",
    "logs_export_failed": "Export failed: %1$s",

    # Settings, second batch
    "settings_search_hint": "Search settings",
    "settings_search_empty": "No matching settings",
    "settings_advanced_expand": "Show advanced settings",
    "settings_advanced_collapse": "Hide advanced settings",
    "settings_auto_connect": "Connect automatically at launch",
    "settings_auto_connect_hint": "When the app opens and VPN authorisation is already granted, bring the tunnel up",
    "settings_restore_defaults": "Restore default settings",
    "settings_restore_defaults_hint": "Added rule sources are cleared as well",
    "settings_restore_confirm_title": "Restore default settings?",
    "settings_restore_confirm_text": "Every setting returns to its initial state, including added rule sources and custom switches. The current connection is not dropped, but reconnecting is needed to use the new parameters.",
    "settings_restore_action": "Restore",
    "settings_restore_done": "Default settings restored",
    "settings_dynamic_color_unsupported": "Not supported on this device (needs Android 12 or above)",

    # Rules, second batch
    "rules_group_counts": "%1$d domains · %2$d addresses",
    "rules_verdict_label": "Last verdict",
    "rules_verdict_cert": "Certificate",
    "rules_verdict_selector": "Selection",
    "rules_verdict_ok": "Usable",
    "rules_verdict_wrong_cert": "Certificate mismatch",
    "rules_verdict_unreachable": "Unreachable",
    "rules_verdict_in_flight": "Probing",
    "rules_verdict_silent": "Connected, no data",
    "rules_verdict_failed": "Failed before",
    "rules_verdict_unprobed": "Not probed",
    "rules_verdict_rtt": "%1$d ms",
    "rules_verdict_domain_bad": "%1$d of %2$d addresses unusable",
    "rules_verdict_domain_all_bad": "All %1$d addresses unusable",
    "rules_verdict_group_bad": "%1$d of %2$d addresses unusable",
    "rules_disable_all_confirm_title": "Disable every group?",
    "rules_disable_all_confirm_text": "This keeps %1$d groups out of the kernel; their traffic will connect directly instead.",
    "rules_disable_all_confirm_ok": "Disable all",

    # Home, third batch
    "home_mode_locked": "Disconnect to switch",
    "home_traffic_title": "Rules and splitting",
    "home_traffic_ratio_label": "Rule hit share",
    "home_traffic_flows_label": "Hits / direct / live flows",
    "home_traffic_idle": "Splitting stats appear once connected",

    # Logs, third batch
    "logs_search_hint": "Search content or tag",
    "logs_copy_all": "Copy all",
    "logs_copied_all": "Copied %1$d lines",
    "logs_back_to_latest": "Back to latest",
    "logs_empty_detail": "Connect once and kernel events will appear here",
    "logs_count": "%1$d lines",
    "logs_no_match": "No matching log entries",
    "logs_no_match_detail": "Try another keyword, or clear the filters",
    "logs_clear_confirm_detail": "This cannot be undone",
    "common_cancel": "Cancel",
    "common_confirm": "OK",
    "common_unknown": "Unknown error",
    "slider_input_title": "Enter a value",
    "slider_input_range": "Range %1$s to %2$s",
    "logs_paused_empty": "Paused",
    "logs_paused_empty_detail": "Resume to show new entries",

    # Rules, third batch
    "rules_filter_all": "All",
    "rules_filter_problems": "Problems only",
    "rules_verdict_offline": "Connect to see address verdicts",
    "rules_empty_detail": "Switch to another source, or add a custom one in settings",
    "rules_filter_problems_empty": "No entries with an abnormal verdict",
    "rules_filter_problems_empty_detail": "Only judged addresses count; unjudged and in-flight ones are not abnormal",

    # Data portability
    "settings_section_data": "Data and about",
    "settings_export": "Export data",
    "settings_export_hint": "Save the settings as a JSON file (wallpaper image excluded)",
    "settings_export_action": "Export",
    "common_output_stream_failed": "Cannot open the output stream",
    "common_input_stream_failed": "Cannot open the input stream",
    "settings_import": "Import data",
    "settings_import_hint": "Restore settings from a JSON file; this overwrites the current ones",
    "settings_import_action": "Import",
    "settings_about": "About",
    "settings_about_hint": "Version, environment and updates",
    "settings_export_done": "Settings exported",
    "settings_export_failed": "Export failed: %1$s",
    "settings_import_confirm_title": "Import settings?",
    "settings_import_confirm_text": "Importing overwrites the current settings with the file values, including added rule sources and custom switches (rule files imported on this device are kept). The current connection is not dropped, but reconnecting is needed to use the new parameters.",
    "settings_import_confirm_ok": "Import",
    "settings_import_done": "Imported %1$d settings",
    "settings_import_done_ignored": "Imported %1$d settings, ignored %2$d unrecognised ones",
    "settings_import_bad_json": "This file is not valid JSON",
    "settings_import_not_ours": "This is not a settings file exported by this app",
    "settings_import_too_new": "This file comes from a newer version; update the app first",
    "settings_import_read_failed": "Could not read the file: %1$s",

    # About
    "about_title": "About",
    "about_section_version": "Version",
    "about_app_version": "App version",
    "about_build_type": "Build type",
    "about_build_debug": "Debug",
    "about_build_release": "Release",
    "about_kernel": "Kernel library",
    "about_kernel_ok": "Loaded",
    "about_environment": "Environment",
    "about_device": "Device",
    "about_section_rules": "Sources",
    "about_rule_source": "Current rule source",
    "about_rule_scale": "Rule set size",
    "about_section_update": "Updates",
    "about_update_check": "Check for updates",
    "about_update_checking": "Checking…",
    "about_update_latest": "Up to date",
    "about_update_available": "Version %1$s is available",
    "about_update_notes": "Release notes",
    "about_update_download": "Go to download",
    "about_update_failed": "Check failed: %1$s",
    "about_update_auto_check": "Check automatically at launch",
    "about_update_auto_check_hint": "At most once a day, and only when there is an update",
    "about_update_channel": "Update channel",
    "about_update_channel_hint": "The beta channel looks for pre-releases only, never stable ones",
    "about_update_channel_stable": "Stable",
    "about_update_channel_beta": "Beta",
    "about_section_project": "Project",
    "about_repo": "Project repository",
    "update_dialog_title": "New version available",
    "update_dialog_version": "Version %1$s",
    "update_dialog_later": "Later",
    "about_close": "Close",

    # Disclaimer
    "disclaimer_title": "Disclaimer",
    "disclaimer_intro": "Please read the notice below before use. This app provides no network egress of any kind, does not decrypt TLS by default, and makes no guarantee about how you use it; continuing means you accept all of its terms.",
    "disclaimer_accept": "I have read and agree",
    "disclaimer_accept_locked": "I have read and agree (%1$d)",
    "disclaimer_decline": "Disagree and exit",

    # Battery reminder
    "battery_title": "Battery policy is not set to unrestricted",
    "battery_body": "The system may reap the tunnel while you are not using it, which shows up as a connection that reports itself up while nothing moves. Set speed's battery policy to \"Unrestricted\", otherwise background survival cannot be guaranteed.",
    "battery_oem_hint": "Some systems also need \"Autostart\" and \"Background running\" enabled separately. Neither has a standard entry point and this app cannot read them, so please confirm them in the system settings yourself.",
    "battery_open_settings": "Open battery settings",

    # Log lines. The kernel's own lines are not here: they come from the Rust
    # side and stay English in both languages.
    "log_kernel_loaded": "Kernel library loaded",
    "log_kernel_unavailable": "Kernel library unavailable: %1$s",
    "log_prefetch_done": "Prefetched rules at launch: %1$d KiB",
    "log_prefetch_failed": "Prefetching rules at launch failed: %1$s",
    "log_auto_connect_no_disclaimer": "Auto-connect at launch skipped: the disclaimer has not been accepted",
    "log_auto_connect_no_vpn": "Auto-connect at launch skipped: no VPN authorisation yet",
    "log_vpn_denied": "The user denied VPN authorisation",
    "log_no_notification_permission": "No notification permission; the foreground service may be restricted by the system",
    "log_prepare_failed": "VpnService.prepare threw: %1$s",
    "log_vpn_already_granted": "VPN already authorised; starting directly",
    "log_vpn_needs_grant": "Authorisation needed; showing the system dialog",
    "log_battery_settings_unavailable": "The battery settings page does not open; cannot guide the user",
    "log_disclaimer_accepted": "The user accepted the disclaimer",
    "log_disclaimer_declined": "The user declined the disclaimer; exiting",
    "log_already_running": "Already running; ignoring the duplicate request",
    "log_rules_loaded": "Rules loaded: %1$d KiB (source %2$s)",
    "log_proxy_listening": "Proxy listening on 127.0.0.1:%1$d",
    "log_kernel_started_vpn": "Kernel started (VPN)",
    "log_kernel_started_root": "Root mode started: %1$d domains → %2$s, proxy listening on 127.0.0.1:%3$d",
    "log_vpn_takeover": "Taking over public traffic; the kernel splits it by the rules",
    "log_rules_hot_swapped": "Rules hot-swapped (%1$d KiB); flows in flight were not interrupted",
    "log_rules_hot_swap_rejected": "The kernel rejected the rule hot-swap: %1$s",
    "log_rules_reload_failed": "Reloading the rules failed: %1$s",
    "log_loop_stop_timeout": "The step loop did not exit within %1$d ms; giving up — the kernel will leak rather than be freed",
    "log_disconnected": "Disconnected",
    "log_vpn_revoked": "The system revoked the VPN",
    "log_root_parse_failed": "Parsing the rule document failed; root mode will not hijack any domain: %1$s",
    "log_root_no_domains": "The rule document contains no domains; root mode started but hijacks no traffic",
    "log_cache_from_older_version": "The rule cache came from an older version; discarded and re-fetched",
    "log_offline_stamp_mismatch": "Offline mode: the cache is from %1$s, which does not match the current source %2$s; refusing to use it (that document is certainly not the selected rule set, and using it would only produce a tunnel that forwards nothing)",
    "log_offline_no_stamp": "Offline mode: the cache carries no source stamp, so it cannot be checked; using it as offline mode requires",
    "log_offline_using_cache": "Offline mode: using the downloaded rules",
    "log_rules_updated": "Rules updated (%1$d KiB, source %2$s)",
    "log_fetch_failed": "Fetching the rules failed: %1$s",
    "log_using_cache": "Using the cached rules",
    "log_cache_mismatch_and_fetch_failed": "The cache's source does not match and the fetch failed too; it cannot be used",
    "log_rules_filtered": "Filtered the rules by %1$d switches: %2$d KiB → %3$d KiB",
    "log_filter_failed": "Filtering the rules failed; using the unfiltered ones this time: %1$s",
    "log_fetching": "Fetching rules: %1$s (candidate %2$d/%3$d, %4$s)",
    "log_candidate_failed": "Candidate %1$d/%2$d failed: %3$s (%4$s)",
    "log_reading_local": "Reading the local rule file: %1$s (%2$d KiB)",
    "log_local_imported": "Imported local rules: %1$s (%2$d KiB, copy %3$s)",
    "log_update_available": "Version %1$s is available (%2$s channel)",
    "log_update_latest": "Up to date (%1$s channel)",
    "log_update_failed": "The update check failed: %1$s (%2$s channel)",
    "log_wallpaper_saved": "Wallpaper saved (%1$s, %2$d KiB, %3$d×%4$d)",
    "log_no_app_for_url": "No app can open %1$s: %2$s",
    "log_parse_failed": "Parsing the rules failed: %1$s",
    "log_import_failed": "Importing the local rules failed: %1$s",

    # Exception messages that reach the screen.
    "err_missing_groups_field": "Missing the groups field",
    "err_offline_no_cache": "Offline, and no rules have been downloaded on this device yet",
    "err_offline_source_mismatch": "Offline mode: the downloaded rules belong to %1$s, which does not match the current selection %2$s; go online to download again, or switch back to %1$s",
    "err_source_unavailable": "The rule source \"%1$s\" is unavailable",
    "err_all_candidates_failed": "All %2$d addresses of the rule source \"%1$s\" failed: %3$s",
    "err_local_file_missing": "The local rule file is gone; import \"%1$s\" again",
    "err_not_a_rule_document": "The content is an HTML page, not a rule document",
    "err_too_few_entries": "The rule source \"%1$s\" parsed to too few entries: %2$d found, at least %3$d required",
    # The long tail.
    "log_battery_unrestricted": "Battery policy: set to unrestricted",
    "log_battery_restricted": "Battery policy: not unrestricted; the system may reap the tunnel",
    "err_kernel_error": "The kernel returned an error",
    "err_ca_fingerprint": "Cannot compute the CA certificate fingerprint (subject hash)",
    "err_root_install_failed": "Root mode install failed (%1$s): %2$s",
    "err_root_step": "step %1$d",
    "err_root_no_su_reply": "su returned nothing",
    "err_su_start_failed": "Cannot start su: %1$s",
    "err_su_timeout": "su timed out (%1$d ms): %2$s",
    "rules_unnamed_group": "Unnamed",
    "rules_local_default_label": "Local rules",
    "err_import_read_failed": "Reading the file failed: %1$s",
    "err_import_too_large": "The file is over %1$d MiB; it does not look like a rule document",
    "err_import_empty": "The file is empty",
    "err_import_not_a_document": "The content is not a rule document",
    "err_import_write_failed": "Writing the local copy failed: %1$s",
    "err_update_no_url": "No update address is configured",
    "err_update_cleartext": "The system does not allow cleartext HTTP; use https:// instead",
    "err_update_too_large": "The response is too large",
    "err_update_missing_fields": "Missing versionCode or url",
    "err_update_version_fallback": "version %1$d",
    "err_update_version_unparsable": "Cannot parse the version number: %1$s",
    "root_refused_title": "Root mode is unavailable",
    "root_refused_text": "Root access was not granted, so the mode was not switched. Allow speed in your system root manager, then pick Root mode again.",
    "home_mode_root_asking": "Requesting root access…",
    "error_hint_rules": "The rules are not ready: refresh them on the Rules page, and switch to another source if that keeps failing.",
    "error_hint_permission": "VPN authorisation is missing: tap the button above and allow it in the system dialog.",
    "error_hint_root_unavailable": "Root access was refused: allow speed in your root manager and pick Root mode again.",
    "error_hint_root_install": "The root takeover did not install: the report names the step that failed. An unwritable hosts file and a refused command both look like this.",
    "error_hint_mitm": "The local reverse proxy did not come up: disconnect and try again. A busy port looks the same.",
    "error_hint_kernel": "The kernel started and then failed: send the report below, it carries the kernel's own error.",
    "error_hint_unknown": "Send the report below: it carries the mode, the rule state and the recent log.",
    "error_report_share": "Export error report",
    "error_report_share_title": "Error report",
    "error_report_share_failed": "Export failed: %1$s",
    "report_title": "Error report",
    "report_time": "Time",
    "report_mode": "Mode",
    "report_reason": "Reason",
    "report_error": "Error",
    "report_rules": "Rules",
    "report_cache": "Rule cache",
    "report_settings": "Kernel settings",
    "report_logs": "Recent log",
    "log_hosts_clobbered_repaired": "The hosts file was rewritten by something else; the rule block has been put back",
    "log_hosts_clobbered_unrepaired": "The hosts file was rewritten by something else and putting the block back failed — root mode is hijacking nothing right now",
}


def escape(text: str) -> str:
    """Escape for a string resource: `&` first, then the quote characters.

    An unescaped apostrophe is a build *error* in Android, not a warning, and
    the message points at the line rather than at the cause.
    """
    return (text.replace("&", "&amp;")
                .replace("'", "\\'")
                .replace('"', '\\"'))


def main() -> int:
    src = io.open(SRC, encoding="utf-8").read()
    out_lines: list[str] = []
    seen: list[str] = []
    problems: list[str] = []

    for line in src.split("\n"):
        m = re.match(r'^(\s*)<string name="([^"]+)">(.*)</string>$', line)
        if not m:
            out_lines.append(line)
            continue
        indent, name, zh = m.group(1), m.group(2), m.group(3)
        seen.append(name)
        if name == "app_name":
            out_lines.append(line)      # the product name is not translated
            continue
        en = EN.get(name)
        if en is None:
            problems.append("no translation: %s" % name)
            out_lines.append(line)
            continue
        # Format specifiers have to survive verbatim, or the build breaks at
        # runtime with a MissingFormatArgumentException instead of here.
        want = sorted(re.findall(r"%(?:\d+\$)?[sd]", zh))
        got = sorted(re.findall(r"%(?:\d+\$)?[sd]", en))
        if want != got:
            problems.append("specifier mismatch in %s: %s vs %s" % (name, want, got))
        out_lines.append('%s<string name="%s">%s</string>' % (indent, name, escape(en)))

    unused = sorted(set(EN) - set(seen))
    if unused:
        problems.append("translated but not in the source: %s" % ", ".join(unused))

    if problems:
        print("REFUSING to write — %d problem(s):" % len(problems))
        for p in problems:
            print("  " + p)
        return 1

    DST.parent.mkdir(parents=True, exist_ok=True)
    io.open(DST, "w", encoding="utf-8", newline="\n").write("\n".join(out_lines))
    print("wrote %s (%d strings)" % (DST.relative_to(ROOT), len(seen)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
