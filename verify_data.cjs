const fs = require('fs');
const path = require('path');

const featuresPath = path.join(__dirname, 'landing/data/features.ts');
const contentDir = path.join(__dirname, 'landing/content');

try {
  // Extract feature IDs from features.ts using regex
  const featuresContent = fs.readFileSync(featuresPath, 'utf8');
  const featureIds = [];
  const regex = /id:\s*"([^"]+)"/g;
  let match;
  while ((match = regex.exec(featuresContent)) !== null) {
    featureIds.push(match[1]);
  }
  console.log('Found feature IDs:', featureIds);

  // Check each content file
  const files = fs.readdirSync(contentDir).filter(f => f.endsWith('.json'));
  
  files.forEach(file => {
    const filePath = path.join(contentDir, file);
    try {
      const content = JSON.parse(fs.readFileSync(filePath, 'utf8'));
      
      if (!content.features) {
        console.error(`ERROR: ${file} is missing "features" key.`);
        return;
      }
      
      if (!Array.isArray(content.features)) {
        console.error(`ERROR: ${file} "features" is not an array.`);
        return;
      }

      console.log(`Checking ${file}... OK (${content.features.length} features)`);
      
      // Optional: Check if all IDs in features.ts exist in content (not strictly required by code, but good practice)
      const contentIds = content.features.map(f => f.id);
      const missing = featureIds.filter(id => !contentIds.includes(id));
      if (missing.length > 0) {
        console.warn(`WARNING: ${file} is missing descriptions for IDs: ${missing.join(', ')}`);
      }

    } catch (e) {
      console.error(`ERROR parsing ${file}: ${e.message}`);
    }
  });

} catch (e) {
  console.error('Script error:', e);
}
