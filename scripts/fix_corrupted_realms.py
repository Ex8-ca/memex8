#!/usr/bin/env python3
"""Fix corrupted realm names in memex8 Qdrant."""
import urllib.request
import json

QDRANT = "http://qdrant:6333"
COLLECTION = "memories"

# Corrupted realm -> clean name
FIXES = {
    "Again Complete Entry-b-b-a-a-b-b-a-a-a-a-b-b-a-b-a-b-a-a-a-b-b-a-a-b-b": "Again Complete Entry",
}

for old_name, new_name in FIXES.items():
    print(f"Fixing: '{old_name[:60]}...' -> '{new_name}'")
    
    # Get all point IDs with the corrupted realm_name
    offset = None
    fixed_count = 0
    while True:
        body = json.dumps({
            "filter": {"must": [{"key": "realm_name", "match": {"value": old_name}}]},
            "limit": 100,
            "with_payload": False,
            "with_vector": False,
        }).encode()
        
        if offset:
            body = json.dumps({
                "filter": {"must": [{"key": "realm_name", "match": {"value": old_name}}]},
                "limit": 100,
                "with_payload": False,
                "with_vector": False,
                "offset": offset,
            }).encode()
        
        req = urllib.request.Request(
            f"{QDRANT}/collections/{COLLECTION}/points/scroll",
            data=body,
            headers={"Content-Type": "application/json"},
            method="POST"
        )
        resp = urllib.request.urlopen(req)
        data = json.loads(resp.read())
        
        points = data["result"]["points"]
        next_offset = data["result"].get("next_page_offset")
        
        if not points:
            break
        
        ids = [p["id"] for p in points]
        
        # Update payload for these points
        update_body = json.dumps({
            "payload": {"realm_name": new_name},
            "points": ids,
        }).encode()
        
        update_req = urllib.request.Request(
            f"{QDRANT}/collections/{COLLECTION}/points/payload",
            data=update_body,
            headers={"Content-Type": "application/json"},
            method="POST"
        )
        update_resp = urllib.request.urlopen(update_req)
        update_data = json.loads(update_resp.read())
        
        status = update_data.get("status", "unknown")
        fixed_count += len(ids)
        print(f"  Updated {len(ids)} points (total: {fixed_count}), status: {status}")
        
        if next_offset is None:
            break
        offset = next_offset
    
    print(f"  Done: {fixed_count} points fixed")

print("\nAll corrupted realm names fixed.")
