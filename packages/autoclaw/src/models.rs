use super::*;
pub(super) fn list(site: Site) -> Vec<Value> {
    let china = site == Site::China;
    [("zai_auto","GLM Auto (路由)","Auto",200000,32000,393216,true),
        ("zai_auto-fast","GLM Auto Fast","Auto-Fast",200000,32000,131072,true),
        ("zaicoding_glm-5.3","GLM-5.3 Coding","GLM-5.3",1000000,131072,307200,false),
        ("zai_glm-5.3-flash","GLM-5.3 Flash","GLM-5.3-Flash",1000000,32000,131072,true)]
        .into_iter().map(|(id,name,cn_name,context,output,cn_output,image)|{
            let mut value=json!({"id":id,"name":if china{cn_name}else{name},"reasoning":true,"input":["text"],"contextWindow":if china{1048576}else{context},"maxTokens":if china{cn_output}else{output},"info":{"toolCalling":true,"pricingKnown":false,"catalogSource":"catalog"}});
            if china&&image {value["input"]=json!(["text","image"]);}
            if china {value["info"]["reasoningEfforts"]=json!(["low","high","max"]);}
            value
        }).collect()
}
