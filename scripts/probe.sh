#!/usr/bin/env bash
# gogo live-API probe — READ-ONLY. Outputs to ./gogo-probe (contains tokens; delete after).
set -uo pipefail
cd "$(dirname "$0")" || exit 1; OUT=gogo-probe; mkdir -p "$OUT"; cd "$OUT" || exit 1
CID=46899977096215655
CSECRET=9d85c43b1482497dbbce61f6e4aa173a433796eeae2ca8c5f6129f2dc4de46d9
REDIRECT='https://embed.gog.com/on_login_success?origin=client'

echo "== Login (Q: code exchange) =="
echo "Open in browser:"
echo "  https://auth.gog.com/auth?client_id=$CID&redirect_uri=$REDIRECT&response_type=code&layout=client2"
read -r -p "Paste the full redirect URL (or just the code): " PASTE
CODE=${PASTE##*code=}; CODE=${CODE%%&*}
[ -z "$CODE" ] && { echo "ERROR: no code"; exit 1; }

curl -sG 'https://auth.gog.com/token' \
  --data-urlencode "client_id=$CID" --data-urlencode "client_secret=$CSECRET" \
  --data-urlencode 'grant_type=authorization_code' \
  --data-urlencode "redirect_uri=$REDIRECT" --data-urlencode "code=$CODE" > token.json

read -r AT EXPIRES < <(python3 -c "import json;d=json.load(open('token.json'));print(d['access_token'],d['expires_in'])")
[ -z "${AT:-}" ] && { echo "ERROR: token exchange failed:"; cat token.json; exit 1; }
echo "Q1 expires_in = $EXPIRES seconds (expect ~3600)"
AUTH="Authorization: Bearer $AT"

echo "== Q2: Bearer on embed endpoints =="
curl -s -H "$AUTH" -o owned.json    -w 'user/data/games          -> %{http_code} (expect 200)\n' 'https://embed.gog.com/user/data/games'
curl -s -H "$AUTH" -o gfp.json      -w 'getFilteredProducts      -> %{http_code} (expect 200)\n' 'https://embed.gog.com/account/getFilteredProducts?mediaType=1&page=1&sortBy=title'
curl -s -H "$AUTH" -o userdata.json -w 'userData.json            -> %{http_code} (expect 200)\n' 'https://embed.gog.com/userData.json'
python3 -c "import json;d=json.load(open('owned.json'));print('owned count:',len(d['owned']))" 2>/dev/null || echo "owned.json unreadable"
python3 -c "import json;d=json.load(open('gfp.json'));print('totalProducts:',d.get('totalProducts'))" 2>/dev/null || true

GID=$(python3 -c "import json;print(json.load(open('owned.json'))['owned'][0])" 2>/dev/null)
echo "Probe game id: $GID"
echo "== Q3/Q9: products?expand=downloads,expanded_dlcs =="
curl -s -H "$AUTH" -o product.json -w 'products/%s -> %{http_code} (expect 200)\n' "https://api.gog.com/products/$GID?expand=downloads,expanded_dlcs"
python3 - <<'PY'
import json
d = json.load(open('product.json'))
inst = d.get('downloads', {}).get('installers', [])
print('installers:', len(inst))
if inst:
    u = inst[0].get('galaxy_downlink_json_url')
    print('galaxy_downlink_json_url:', 'PRESENT' if u else 'MISSING')
    print('entry fields:', sorted(inst[0].keys()))
    print('updated flag (Q7):', inst[0].get('updated'), '| version/date:', inst[0].get('version'), inst[0].get('date'))
    open('dl_url.txt', 'w').write(u or '')
print('expanded_dlcs (Q9):', len(d.get('expanded_dlcs', [])))
PY

DL=$(cat dl_url.txt 2>/dev/null || true)
echo "== Q3: downlink JSON endpoint auth =="
if [ -n "$DL" ]; then
  curl -s -o /dev/null -w 'downlink JSON WITHOUT auth -> %{http_code} (expect 401/403)\n' "$DL"
  curl -s -H "$AUTH" -o downlink.json -w 'downlink JSON with Bearer  -> %{http_code} (expect 200)\n' "$DL"
fi
python3 - <<'PY'
import json
try:
    d = json.load(open('downlink.json'))
    print('downlink.json keys:', sorted(d.keys()))
    open('cdn_url.txt', 'w').write(d.get('downlink', ''))
    open('checksum_url.txt', 'w').write(d.get('checksum', ''))
except Exception as e:
    print('downlink.json parse:', e)
PY
CDN=$(cat cdn_url.txt 2>/dev/null || true); CKS=$(cat checksum_url.txt 2>/dev/null || true)

echo "== Q4: CDN file auth (Range GET of first 1 KB) =="
if [ -n "$CDN" ]; then
  curl -s -o sample.bin -w 'CDN GET with Bearer  -> %{http_code} (expect 206/200)\n' -r 0-1023 -H "$AUTH" "$CDN"
  curl -s -o /dev/null -w 'CDN GET without auth -> %{http_code} (info: URL is pre-tokenized)\n' -r 0-1023 "$CDN"
  ls -l sample.bin
fi

echo "== Q5: checksum XML sample =="
if [ -n "$CKS" ]; then
  curl -s -o checksum.xml -w 'checksum XML -> %{http_code} (expect 200)\n' "$CKS"
  head -c 700 checksum.xml; echo
fi

echo "== Q6: gentle rate probe (10 HEADs, 0.5s apart; watch for 429/5xx) =="
if [ -n "$CDN" ]; then
  for _ in $(seq 1 10); do curl -s -o /dev/null -w '%{http_code} ' -r 0-0 "$CDN"; sleep 0.5; done; echo
fi

echo "== Q8: movies in library =="
curl -s -H "$AUTH" -o movies.json -w 'getFilteredProducts mediaType=2 -> %{http_code}\n' 'https://embed.gog.com/account/getFilteredProducts?mediaType=2&page=1'
python3 -c "import json;d=json.load(open('movies.json'));print('movies totalProducts:',d.get('totalProducts'))" 2>/dev/null || true

echo "== Q10 (A1): hidden-game semantics (isHidden / hasHiddenProducts) =="
python3 - <<'PY'
import json
d = json.load(open('gfp.json'))
hid = [p for p in d['products'] if p.get('isHidden')]
print('page1: %d products, isHidden=true: %d, hasHiddenProducts: %s'
      % (len(d['products']), len(hid), d.get('hasHiddenProducts')))
PY
echo "-- trying params that might surface hidden products:"
for q in 'hidden=1' 'hidden=true' 'showHidden=1' 'isHidden=1' 'hideHidden=0' 'filter=hidden' 'hiddenFlag=1' 'hiddenFlag=true'; do
  curl -s -H "$AUTH" -o gfp_h.json "https://embed.gog.com/account/getFilteredProducts?mediaType=1&$q"
  python3 -c "import json;d=json.load(open('gfp_h.json'));hid=[p for p in d.get('products',[]) if p.get('isHidden')];print('  %-14s -> totalProducts=%s isHidden-in-page=%d hasHiddenProducts=%s' % ('$q', d.get('totalProducts'), len(hid), d.get('hasHiddenProducts')))" 2>/dev/null || echo "  $q -> unparseable response"
done

echo "== Q11 (C4): extras (bonus_content) filename sources =="
XDL=""; XGID=""
for gid in $(python3 -c "import json,os;print(' '.join(map(str,json.load(open('owned.json'))['owned'][:int(os.environ.get('Q11_SCAN','200'))])))"); do
  curl -s -H "$AUTH" -o "prod_$gid.json" "https://api.gog.com/products/$gid?expand=downloads"
  XDL=$(python3 -c "import json;d=json.load(open('prod_$gid.json'));bc=d.get('downloads',{}).get('bonus_content',[]);print(bc[0].get('galaxy_downlink_json_url','') if bc else '')")
  if [ -n "$XDL" ]; then XGID=$gid; echo "bonus_content found on game $gid"; break; fi
  sleep 0.3
done
if [ -n "$XDL" ]; then
  echo "-- bonus_content entry keys: $(python3 -c "import json;d=json.load(open('prod_$XGID.json'));print(sorted(d['downloads']['bonus_content'][0].keys()))")"
  curl -s -H "$AUTH" -o extras_downlink.json "$XDL"
  python3 - <<'PY'
import json
d = json.load(open('extras_downlink.json'))
print('-- extras downlink keys:', sorted(d.keys()))
open('x_cdn.txt', 'w').write(d.get('downlink', ''))
open('x_cks.txt', 'w').write(d.get('checksum', ''))
PY
  XCDN=$(cat x_cdn.txt 2>/dev/null); XCKS=$(cat x_cks.txt 2>/dev/null)
  echo "-- CDN response headers (1-byte Range GET, no auth — URL is pre-tokenized):"
  if [ -n "$XCDN" ]; then
    curl -s -D - -o /dev/null -r 0-0 "$XCDN" | tr -d '\r' | grep -iE '^(HTTP/|content-disposition|content-type|content-length)'
  fi
  if [ -n "$XCKS" ]; then
    echo "-- checksum XML present; first 300 B (looking for name= attr):"
    curl -s "$XCKS" | head -c 300; echo
  else
    echo "-- NO checksum url for this extra: filename must come from Content-Disposition or entry name"
  fi
else
  echo "no bonus_content found in the scanned games (Q11_SCAN=${Q11_SCAN:-200}) — the library may have none; C4 falls back to the designed chain"
fi

echo "== Token refresh rotation =="
RT=$(python3 -c "import json;print(json.load(open('token.json'))['refresh_token'])")
curl -sG 'https://auth.gog.com/token' \
  --data-urlencode "client_id=$CID" --data-urlencode "client_secret=$CSECRET" \
  --data-urlencode 'grant_type=refresh_token' --data-urlencode "refresh_token=$RT" > refresh.json
python3 - <<'PY'
import json
try:
    a = json.load(open('token.json')); b = json.load(open('refresh.json'))
    print('refresh_token rotated:', a['refresh_token'] != b.get('refresh_token'))
    print('new expires_in:', b.get('expires_in'))
except Exception as e:
    print('refresh parse:', e)
PY

echo "DONE. Review outputs, then: rm -rf $OUT   (it contains your tokens)"
