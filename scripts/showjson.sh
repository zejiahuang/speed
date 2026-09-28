#!/bin/bash
P=$(command -v php)
$P -r '
$j = json_decode(file_get_contents("/tmp/rp_fetch_out.json"), true);
if (!is_array($j)) { echo "非 JSON\n"; exit; }
echo "顶层键: ", implode(", ", array_keys($j)), "\n\n";
foreach ($j as $k => $v) {
    if (is_array($v)) {
        echo "$k => {", implode(", ", array_keys($v)), "}\n";
    } else {
        echo "$k => ", var_export($v, true), "\n";
    }
}
'
