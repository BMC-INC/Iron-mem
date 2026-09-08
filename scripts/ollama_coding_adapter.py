#!/usr/bin/env python3
"""Local-only Ollama coding adapter. Refuses remote models and non-loopback URLs."""
import json
import sys
import urllib.request
from urllib.parse import urlsplit


def main():
    payload=json.load(sys.stdin)
    settings=payload.get('settings',{})
    endpoint=settings.get('endpoint','http://127.0.0.1:11434')
    parsed=urlsplit(endpoint)
    if parsed.scheme!='http' or parsed.hostname not in ('127.0.0.1','localhost','::1') or parsed.username or parsed.password or parsed.path not in ('','/') or parsed.query or parsed.fragment:
        raise ValueError('only plain loopback Ollama endpoints are allowed')
    opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
    def request(path,body=None):
        data=None if body is None else json.dumps(body).encode()
        req=urllib.request.Request(endpoint.rstrip('/')+path,data=data,headers={'Content-Type':'application/json'})
        with opener.open(req,timeout=180) as response: return json.load(response)
    models=request('/api/tags')['models']
    model=next((m for m in models if m['name']==payload['model']),None)
    if not model or model.get('remote_host') or model.get('remote_model') or ':cloud' in model['name']:
        raise ValueError('requested model must already exist locally and must not be cloud-backed')
    expected=settings.get('model_digest')
    if not expected or model['digest']!=expected: raise ValueError('local model digest mismatch')
    prompt='Implement exactly the requested function name and signature in Python 3. Use only the Python standard library and supplied starter modules. Return the requested Python value from the function; do not serialize that value as JSON. The outer JSON is only a transport envelope for your source file. Return JSON {"files":{"solution.py":"complete Python source"}} only. Use the supplied project history when available; corrections override earlier assumptions. Do not access files or networks.\n'+json.dumps({k:payload[k] for k in ('instruction','files','context')})
    options={'temperature':settings.get('temperature',0),'seed':payload['seed'],
             'num_ctx':settings.get('num_ctx',4096),'num_predict':settings.get('num_predict',1024)}
    response=request('/api/generate',{'model':model['name'],'prompt':prompt,'stream':False,'format':{
        'type':'object','properties':{'files':{'type':'object','properties':{'solution.py':{'type':'string'}},'required':['solution.py'],'additionalProperties':False}},
        'required':['files'],'additionalProperties':False},'options':options,'think':False,'keep_alive':'10m'})
    result=json.loads(response['response'])
    result['measurement']={key:response.get(key) for key in ('total_duration','load_duration','prompt_eval_count','eval_count','done_reason')}
    result['measurement'].update({'model_digest':model['digest'],'ollama_version':request('/api/version')['version'],'options':options})
    print(json.dumps(result))


if __name__=='__main__': main()
